<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // The separate fleet video window's route (backend `fleet_video_window_open` opens it as `/fleet-video`).
  // Deliberately thin, like /video: no map, no telemetry — the main window sends the tile model.
  import FleetVideoWindow from '$lib/components/video/FleetVideoWindow.svelte';
</script>

<FleetVideoWindow />
