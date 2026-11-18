<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // Fleet video settings (stores/fleetVideo): the URL template every drone's stream follows, per-vehicle
  // overrides, and whether the video views follow the active vehicle. Shown in the RTSP section of the
  // Video panel when the fleet features are on.
  import { t } from 'svelte-i18n';
  import Toggle from '$lib/components/panel/Toggle.svelte';
  import { vehicles, activeVehicleId } from '$lib/stores/vehicles';
  import { resolveStreamUrl } from '$lib/helpers/fleetVideoUrl';
  import {
    fleetVideoConfig, discoveredStreams, setFleetVideoFollow, setFleetVideoTemplate, setVehicleVideoOverride,
    vehicleVideoOverride,
  } from '$lib/stores/fleetVideo';

  const rows = $derived(
    [...$vehicles.values()]
      .sort((a, b) => a.linkId - b.linkId || a.sysid - b.sysid)
      .map((v) => ({
        v,
        override: vehicleVideoOverride($fleetVideoConfig, v),
        // What the template (or else the drone's camera) gives this vehicle — the override field's placeholder.
        fromTemplate: resolveStreamUrl({ ...$fleetVideoConfig, overrides: {} }, v, 'main', $discoveredStreams.get(v.vehicleId)),
        active: v.vehicleId === $activeVehicleId,
      })),
  );

  function inputVal(e: Event): string {
    return (e.currentTarget as HTMLInputElement).value;
  }
</script>

<div class="fv">
  <span class="label section">{$t('video.fleet.title')}</span>

  <div class="field-row" title={$t('video.fleet.followHint')}>
    <Toggle checked={$fleetVideoConfig.followActive} onchange={(c) => setFleetVideoFollow(c)} id="fv-follow" />
    <span class="label">{$t('video.fleet.follow')}</span>
  </div>

  <label class="field">
    <span class="label">{$t('video.fleet.template')}</span>
    <input
      class="text-input"
      type="text"
      placeholder={'rtsp://10.10.0.{sysid}:8554/{stream}'}
      value={$fleetVideoConfig.template}
      onchange={(e) => setFleetVideoTemplate(inputVal(e))}
    />
    <p class="hint">{$t('video.fleet.templateHint')}</p>
  </label>

  {#if rows.length}
    <div class="fv-list">
      {#each rows as r (r.v.vehicleId)}
        <div class="fv-row" class:active={r.active}>
          <span class="fv-name" title={r.v.vehicleId}>{r.v.name}</span>
          <input
            class="text-input fv-url"
            type="text"
            placeholder={r.fromTemplate ?? $t('video.fleet.noStream')}
            value={r.override}
            title={$t('video.fleet.overrideHint')}
            onchange={(e) => setVehicleVideoOverride(r.v, inputVal(e))}
          />
        </div>
      {/each}
    </div>
  {:else}
    <p class="hint">{$t('video.fleet.noVehicles')}</p>
  {/if}
</div>

<style>
  .fv { display: flex; flex-direction: column; gap: 8px; padding-top: 4px; border-top: 1px solid #444; }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field-row { display: flex; align-items: center; gap: 8px; }
  .label { font-size: 12px; color: #aaa; }
  .label.section { color: #ccc; font-weight: 600; }
  .hint { font-size: 11px; color: #777; margin: 0; }
  .text-input {
    height: 28px;
    padding: 0 8px;
    background: #434343;
    color: #e0e0e0;
    border: 1px solid #555;
    border-radius: 4px;
    font-size: 12px;
    min-width: 0;
  }
  .fv-list { display: flex; flex-direction: column; gap: 4px; max-height: 220px; overflow-y: auto; }
  .fv-row { display: flex; align-items: center; gap: 6px; }
  .fv-row.active .fv-name { color: #37a8db; }
  .fv-name { flex: 0 0 34%; font-size: 12px; color: #ddd; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fv-url { flex: 1; }
</style>
