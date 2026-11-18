<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // Fleet video grid ("video wall"): a live preview tile per drone (FleetVideoTile). Tiles stream only while
  // visible; clicking one makes that drone active. Preview URLs come from the fleet video template / per-vehicle
  // overrides / the drone's camera (Video panel → Fleet video).
  //
  // "Selected" view (pick a few): only the group-selected drones, as a strip above the map, next to the
  // full-quality main video of the ACTIVE drone (the regular video window, which follows the active vehicle).
  // "Window" opens the same wall as a separate OS window, for a second display (controllers/fleetVideoWindow).
  import Button from '$lib/components/panel/Button.svelte';
  import { selectedVehicleIds } from '$lib/stores/fleetSelection';
  import { videoState, toggleVideo } from '$lib/stores/video';
  import { t } from 'svelte-i18n';
  import PanelShell from '$lib/components/panel/PanelShell.svelte';
  import SegmentedToggle from '$lib/components/panel/SegmentedToggle.svelte';
  import FleetVideoTile from './FleetVideoTile.svelte';
  import { vehicles, activeVehicleId } from '$lib/stores/vehicles';
  import { allTelemetry } from '$lib/stores/telemetry';
  import { vehicleStreams } from '$lib/stores/fleetVideo';
  import { switchVehicle } from '$lib/controllers/connectionController';
  import { buildTiles } from '$lib/helpers/fleetVideoTiles';
  import { fleetVideoWindowOpen, openFleetVideoWindow } from '$lib/controllers/fleetVideoWindow';
  import { isMobile } from '$lib/platform';
  import { panelState, type FleetVideoTile as TileSize, type FleetVideoView } from '$lib/stores/panelState';

  const SIZES: Record<TileSize, number> = { s: 200, m: 280, l: 400 };

  const size = $derived($panelState.fleetVideoTile);
  const view = $derived($panelState.fleetVideoView);
  const all = $derived(
    buildTiles($vehicles, $allTelemetry, (id) => $vehicleStreams.get(id)?.preview ?? null, $selectedVehicleIds),
  );
  const tiles = $derived(view === 'selected' ? all.filter((x) => x.selected) : all);
  const anyPreview = $derived(tiles.some((x) => x.source));
</script>

<PanelShell variant={view === 'selected' ? 'wide-compact' : 'fullscreen'} title={$t('video.fleet.gridTitle')}>
  {#snippet headerActions()}
    <SegmentedToggle
      size="sm"
      value={view}
      options={[
        { value: 'all', label: $t('video.fleet.viewAll') },
        { value: 'selected', label: $t('video.fleet.viewSelected', { values: { n: $selectedVehicleIds.size } }) },
      ]}
      onchange={(v) => panelState.patch({ fleetVideoView: v as FleetVideoView })}
    />
    {#if $videoState.kind === 'rtsp'}
      <Button size="sm" variant="mode" active={$videoState.enabled} title={$t('video.fleet.mainVideoHint')} onclick={toggleVideo}>
        {$t('video.fleet.mainVideo')}
      </Button>
    {/if}
    <SegmentedToggle
      size="sm"
      value={size}
      options={[
        { value: 's', label: $t('video.fleet.sizeS') },
        { value: 'm', label: $t('video.fleet.sizeM') },
        { value: 'l', label: $t('video.fleet.sizeL') },
      ]}
      onchange={(v) => panelState.patch({ fleetVideoTile: v as TileSize })}
    />
    {#if !isMobile}
      <Button size="sm" variant="mode" active={$fleetVideoWindowOpen} title={$t('video.fleet.windowHint')}
        onclick={() => void openFleetVideoWindow()}>
        {$t('video.fleet.window')}
      </Button>
    {/if}
  {/snippet}
  {#snippet body()}
    {#if view === 'selected' && tiles.length === 0}
      <p class="hint">{$t('video.fleet.selectHint')}</p>
    {:else if !anyPreview}
      <p class="hint">{$t('video.fleet.gridEmpty')}</p>
    {/if}
    <div class="grid" class:strip={view === 'selected'} style:--tile="{SIZES[size]}px">
      {#each tiles as tile (tile.vehicleId)}
        <FleetVideoTile {tile} active={tile.vehicleId === $activeVehicleId} onselect={(id) => void switchVehicle(id)} />
      {/each}
    </div>
  {/snippet}
</PanelShell>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(var(--tile), 1fr));
    gap: 8px;
    padding: 4px;
  }
  /* Selected view: one row under the panel header, scrolling sideways — the map and the main video stay visible. */
  .grid.strip { display: flex; overflow-x: auto; }
  .grid.strip > :global(*) { flex: 0 0 var(--tile); }
  .hint { font-size: 12px; color: #888; margin: 4px 4px 10px; }
</style>
