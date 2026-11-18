<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // One drone's live preview tile (Fleet video panel, fleet video window). Watches its preview stream only
  // while it is on screen (IntersectionObserver → stores/videoPreviews lease), so off-screen drones cost no
  // radio airtime. Name in the drone's map colour, mode / armed / battery below; a click makes it the active
  // vehicle, whose main stream the big video views then follow (stores/fleetVideo).
  import { t } from 'svelte-i18n';
  import { previews, acquirePreview } from '$lib/stores/videoPreviews';
  import type { FleetVideoTileModel } from '$lib/helpers/fleetVideoTiles';

  let { tile, active = false, onselect }: {
    tile: FleetVideoTileModel;
    active?: boolean;
    onselect: (vehicleId: string) => void;
  } = $props();

  const streamId = $derived(`${tile.vehicleId}:preview`);
  const preview = $derived($previews.get(streamId));
  let root = $state<HTMLElement>();
  let visible = $state(false);

  $effect(() => {
    const el = root;
    if (!el) return;
    const io = new IntersectionObserver((entries) => { visible = entries.some((e) => e.isIntersecting); });
    io.observe(el);
    return () => io.disconnect();
  });

  $effect(() => {
    const source = tile.source;
    if (!visible || !source) return;
    return acquirePreview(streamId, source);
  });
</script>

<button
  class="tile"
  class:active
  bind:this={root}
  style:--vc={tile.color}
  title={$t('video.fleet.tileHint', { values: { name: tile.name } })}
  onclick={() => onselect(tile.vehicleId)}
>
  <div class="frame">
    {#if preview?.status === 'live' && preview.url}
      <img src={preview.url} alt={tile.name} draggable="false" />
    {:else}
      <span class="msg" class:err={preview?.status === 'error'}>
        {#if !tile.source}{$t('video.fleet.noPreview')}
        {:else if preview?.status === 'error'}{preview.error}
        {:else}{$t('video.fleet.connecting')}{/if}
      </span>
    {/if}
  </div>
  <div class="bar">
    <span class="dot"></span>
    <span class="name">{tile.name}</span>
    <span class="mode">{tile.mode}</span>
    <span class="arm" class:armed={tile.armed}>{tile.armed ? $t('fleet.armed') : $t('fleet.disarmed')}</span>
    <span class="bat">{tile.battery}</span>
  </div>
</button>

<style>
  .tile {
    display: flex;
    flex-direction: column;
    padding: 0;
    background: #1e1e1e;
    border: 2px solid #3a3a3a;
    border-radius: 6px;
    overflow: hidden;
    cursor: pointer;
    color: #ddd;
    text-align: left;
    min-width: 0;
  }
  .tile:hover { border-color: #5a5a5a; }
  .tile.active { border-color: #37a8db; }
  .frame {
    position: relative;
    aspect-ratio: 16 / 9;
    background: #111;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .frame img { width: 100%; height: 100%; object-fit: contain; display: block; }
  .msg { font-size: 11px; color: #888; padding: 0 8px; text-align: center; }
  .msg.err { color: #e0a53c; }
  .bar { display: flex; align-items: center; gap: 6px; padding: 4px 6px; font-size: 11px; min-width: 0; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--vc); flex: 0 0 auto; }
  .name { font-weight: 600; color: var(--vc); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mode { color: #bbb; white-space: nowrap; }
  .arm { color: #888; white-space: nowrap; }
  .arm.armed { color: #e05050; font-weight: 600; }
  .bat { margin-left: auto; color: #bbb; white-space: nowrap; }
</style>
