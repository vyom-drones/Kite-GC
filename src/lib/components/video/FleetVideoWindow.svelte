<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // The separate fleet video window (route /fleet-video): the fleet's live preview tiles filling a window you
  // can put on a second display while the main window shows the map. The main window sends the tile model
  // (controllers/fleetVideoWindow); this page pulls the preview streams itself and makes a clicked drone active
  // through the backend, so the main window follows. View and tile size are this window's own.
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, emitTo } from '@tauri-apps/api/event';
  import SegmentedToggle from '$lib/components/panel/SegmentedToggle.svelte';
  import FleetVideoTile from './FleetVideoTile.svelte';
  import { MODEL_EVENT, READY_EVENT, type FleetVideoModel } from '$lib/helpers/fleetVideoWindowEvents';

  type View = 'all' | 'selected';
  type Size = 's' | 'm' | 'l';
  const SIZES: Record<Size, number> = { s: 220, m: 320, l: 480 };
  const PREFS_KEY = 'kite-gc-fleet-video-window';

  function loadPrefs(): { view: View; size: Size } {
    try {
      const p = JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}') as { view?: string; size?: string };
      return {
        view: p.view === 'selected' ? 'selected' : 'all',
        size: p.size === 's' || p.size === 'l' ? p.size : 'm',
      };
    } catch {
      return { view: 'all', size: 'm' };
    }
  }

  const prefs = loadPrefs();
  let view = $state<View>(prefs.view);
  let size = $state<Size>(prefs.size);
  let model = $state<FleetVideoModel | null>(null);

  $effect(() => {
    try { localStorage.setItem(PREFS_KEY, JSON.stringify({ view, size })); } catch { /* blocked storage */ }
  });

  const selectedTiles = $derived(model ? model.tiles.filter((x) => x.selected) : []);
  const tiles = $derived(view === 'selected' ? selectedTiles : (model?.tiles ?? []));

  onMount(() => {
    let unlisten: (() => void) | null = null;
    void listen<FleetVideoModel>(MODEL_EVENT, (e) => { model = e.payload; }).then((u) => {
      unlisten = u;
      void emitTo('main', READY_EVENT).catch(() => {});
    });
    return () => unlisten?.();
  });

  function select(vehicleId: string): void {
    void invoke('set_active_vehicle', { vehicleId }).catch(() => {});
  }
</script>

<div class="fvw">
  <header>
    <span class="title">{$t('video.fleet.gridTitle')}</span>
    <SegmentedToggle
      size="sm"
      value={view}
      options={[
        { value: 'all', label: $t('video.fleet.viewAll') },
        { value: 'selected', label: $t('video.fleet.viewSelected', { values: { n: selectedTiles.length } }) },
      ]}
      onchange={(v) => (view = v as View)}
    />
    <SegmentedToggle
      size="sm"
      value={size}
      options={[
        { value: 's', label: $t('video.fleet.sizeS') },
        { value: 'm', label: $t('video.fleet.sizeM') },
        { value: 'l', label: $t('video.fleet.sizeL') },
      ]}
      onchange={(v) => (size = v as Size)}
    />
  </header>
  <main>
    {#if !model}
      <p class="hint">{$t('video.fleet.windowWaiting')}</p>
    {:else if view === 'selected' && tiles.length === 0}
      <p class="hint">{$t('video.fleet.selectHint')}</p>
    {:else if !tiles.some((x) => x.source)}
      <p class="hint">{$t('video.fleet.gridEmpty')}</p>
    {/if}
    <div class="grid" style:--tile="{SIZES[size]}px">
      {#each tiles as tile (tile.vehicleId)}
        <FleetVideoTile {tile} active={tile.vehicleId === model?.activeId} onselect={select} />
      {/each}
    </div>
  </main>
</div>

<style>
  :global(html), :global(body) { margin: 0; height: 100%; background: #161616; }
  .fvw { display: flex; flex-direction: column; height: 100vh; color: #ddd; font-family: system-ui, sans-serif; }
  header { display: flex; align-items: center; gap: 10px; padding: 8px 12px; background: #242424; border-bottom: 1px solid #333; }
  .title { font-size: 14px; font-weight: 600; margin-right: auto; }
  main { flex: 1; overflow-y: auto; padding: 8px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(var(--tile), 1fr)); gap: 8px; }
  .hint { font-size: 13px; color: #888; margin: 4px 4px 12px; }
</style>
