// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet video tiles — what each drone's tile shows, built once for the Fleet video panel and the separate
// fleet video window (controllers/fleetVideoWindow sends this model there; that window has no telemetry of
// its own).

import type { VehicleSummary } from '$lib/stores/vehicles';
import type { TelemetryData } from '$lib/stores/telemetry';
import { isVehicleArmed, orderedVehicles, vehicleColorFor } from './fleetStatus';

export interface FleetVideoTileModel {
  vehicleId: string;
  name: string;
  color: string;
  /** Preview RTSP URL; null = the drone has none. */
  source: string | null;
  mode: string;
  armed: boolean;
  battery: string;
  /** In the group selection (the "Selected" view). */
  selected: boolean;
}

export function batteryLabel(tv: TelemetryData | undefined): string {
  if (!tv || tv.lastUpdate <= 0) return '—';
  return tv.batteryPercentage > 0 ? `${Math.round(tv.batteryPercentage)}%` : `${tv.voltage.toFixed(1)} V`;
}

export function buildTiles(
  known: ReadonlyMap<string, VehicleSummary>,
  telem: ReadonlyMap<string, TelemetryData>,
  previewUrl: (vehicleId: string) => string | null,
  selected: ReadonlySet<string>,
): FleetVideoTileModel[] {
  const ordered = orderedVehicles(known);
  return ordered.map((v) => {
    const tv = telem.get(v.vehicleId);
    return {
      vehicleId: v.vehicleId,
      name: v.name,
      color: vehicleColorFor(ordered, v.vehicleId),
      source: previewUrl(v.vehicleId),
      mode: tv?.flightMode.primary || '—',
      armed: tv ? isVehicleArmed(tv) : false,
      battery: batteryLabel(tv),
      selected: selected.has(v.vehicleId),
    };
  });
}
