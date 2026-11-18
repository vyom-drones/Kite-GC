// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet map markers ("3D model" style) — bounded drawing cost for many vehicles.
//
// Rasterising a vehicle's model top-down (`renderUavTopDown`: z-buffered, 2× super-sampled) takes
// milliseconds per call. Redrawing every fleet marker on every map update pinned the UI thread with 20
// vehicles. Instead each vehicle's model is rasterised once at heading 0 into a sprite and the marker only
// ROTATES that sprite: the renderer applies yaw last, about the axis the top-down camera looks along, so
// a rotated heading-0 render is the heading render (shading aside: the light is world-fixed).
//
//  • type / colour / size change → re-raster now (the sprite would be wrong);
//  • pitch / roll change         → re-raster in TILT_STEP_DEG steps, at most RASTERS_PER_SEC per second
//                                  across the whole fleet (the token budget); until then the last sprite;
//  • heading change              → rotate only, never a raster;
//  • nothing visible changed     → no canvas work at all.
//
// No runtime imports: the renderer and the canvas factory are passed in (testable without a DOM).

import type { UavMesh } from './uavMesh';

export const TILT_STEP_DEG = 5;
export const RASTERS_PER_SEC = 8;

export type RenderTopDown = (
  ctx: CanvasRenderingContext2D, mesh: UavMesh, size: number,
  headingDeg: number, pitchDeg: number, rollDeg: number,
  tint?: [number, number, number], tintAmount?: number,
) => void;

/** Quantize an angle to `step` degrees (−0 normalised to 0, so keys stay stable around level). */
export function quantize(deg: number, step: number): number {
  return Math.round(deg / step) * step || 0;
}

/** Token bucket: `perSec` refill, `burst` capacity. The clock is passed in. */
export class RasterBudget {
  private tokens: number;
  private last: number | null = null;
  constructor(private readonly perSec: number, private readonly burst: number) {
    this.tokens = burst;
  }
  take(nowMs: number): boolean {
    if (this.last !== null) this.tokens = Math.min(this.burst, this.tokens + ((nowMs - this.last) / 1000) * this.perSec);
    this.last = nowMs;
    if (this.tokens < 1) return false;
    this.tokens -= 1;
    return true;
  }
}

interface Sprite { identity: string; tilt: string; canvas: HTMLCanvasElement }

export class FleetModelSprites {
  private readonly sprites = new Map<string, Sprite>();
  /** What each marker canvas currently shows; a replaced canvas (setIcon) has no entry → redrawn. */
  private readonly shown = new WeakMap<HTMLCanvasElement, string>();
  /** Raster calls so far (diagnostics / tests). */
  rasters = 0;

  constructor(
    private readonly render: RenderTopDown,
    private readonly createCanvas: (size: number) => HTMLCanvasElement,
    private readonly budget = new RasterBudget(RASTERS_PER_SEC, RASTERS_PER_SEC),
  ) {}

  /** Draw vehicle `id`'s model into its marker canvas (`target`, square). */
  draw(
    id: string, target: HTMLCanvasElement, mesh: UavMesh, platformType: number,
    headingDeg: number, pitchDeg: number, rollDeg: number,
    color: string, tint: [number, number, number], tintAmount: number, nowMs: number,
  ): void {
    const size = target.width;
    const identity = `${platformType}|${color}|${size}`;
    const p = quantize(pitchDeg, TILT_STEP_DEG), r = quantize(rollDeg, TILT_STEP_DEG);
    const tilt = `${p}|${r}`;
    let s = this.sprites.get(id);
    if (!s || s.identity !== identity || (s.tilt !== tilt && this.budget.take(nowMs))) {
      const canvas = s && s.identity === identity ? s.canvas : this.createCanvas(size);
      const sctx = canvas.getContext('2d');
      if (!sctx) return;
      this.render(sctx, mesh, size, 0, p, r, tint, tintAmount);
      this.rasters++;
      s = { identity, tilt, canvas };
      this.sprites.set(id, s);
    }
    const heading = Math.round(headingDeg);
    const view = `${s.identity}|${s.tilt}|${heading}`;
    if (this.shown.get(target) === view) return;
    const ctx = target.getContext('2d');
    if (!ctx) return;
    ctx.clearRect(0, 0, size, size);
    ctx.save();
    ctx.translate(size / 2, size / 2);
    ctx.rotate((heading * Math.PI) / 180);
    ctx.drawImage(s.canvas, -size / 2, -size / 2);
    ctx.restore();
    this.shown.set(target, view);
  }

  /** Drop a vehicle that left the map. */
  forget(id: string): void {
    this.sprites.delete(id);
  }
}
