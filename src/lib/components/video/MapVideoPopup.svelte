<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // A drone's live preview over its map marker (2D: inside a Leaflet popup, 3D: an overlay at the cursor).
  // Holds a preview lease while shown (stores/videoPreviews), so hovering a marker starts that drone's
  // preview and moving away stops it after the idle delay.
  import { t } from 'svelte-i18n';
  import { previews, acquirePreview } from '$lib/stores/videoPreviews';

  let { vehicleId, name, color, source }: { vehicleId: string; name: string; color: string; source: string } = $props();

  const streamId = $derived(`${vehicleId}:preview`);
  const preview = $derived($previews.get(streamId));

  $effect(() => acquirePreview(streamId, source));
</script>

<div class="mvp" style:--vc={color}>
  <div class="frame">
    {#if preview?.status === 'live' && preview.url}
      <img src={preview.url} alt={name} draggable="false" />
    {:else}
      <span class="msg" class:err={preview?.status === 'error'}>
        {preview?.status === 'error' ? preview.error : $t('video.fleet.connecting')}
      </span>
    {/if}
  </div>
  <div class="name">{name}</div>
</div>

<style>
  .mvp { width: 240px; background: #1e1e1e; border: 2px solid var(--vc); border-radius: 6px; overflow: hidden; }
  .frame { aspect-ratio: 16 / 9; background: #111; display: flex; align-items: center; justify-content: center; }
  .frame img { width: 100%; height: 100%; object-fit: contain; display: block; }
  .msg { font-size: 11px; color: #888; padding: 0 8px; text-align: center; }
  .msg.err { color: #e0a53c; }
  .name { font-size: 11px; font-weight: 600; color: var(--vc); padding: 3px 6px; }
</style>
