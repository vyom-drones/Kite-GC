<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (C) 2026 vyom
-->

<script lang="ts">
  // Quick actions for the ACTIVE vehicle — one grid on the map for the commands an operator needs at once:
  // RTL and Hold are one tap; Land, Takeoff and Arm/Disarm are hold-to-confirm (a stray tap must not land or
  // disarm an aircraft). The altitude row changes the target altitude (Guided reposition at the current
  // position) and doubles as the takeoff altitude on the ground. Follows the Fleet selection: whatever
  // vehicle is active is commanded. Commands and their feedback are the control panel's
  // (controllers/vehicleControl).
  import { t } from 'svelte-i18n';
  import Button from '$lib/components/panel/Button.svelte';
  import HoldToConfirm from '$lib/components/panel/HoldToConfirm.svelte';
  import NumberStepper from '$lib/components/NumberStepper.svelte';
  import type { TelemetryData } from '$lib/stores/telemetry';
  import { activeVehicle } from '$lib/stores/vehicles';
  import { resolveIntentMode } from '$lib/helpers/fleetStatus';
  import {
    controlAvailable, isArmed, busyAction, lastFeedback, rtl, land, takeoff, arm, disarm, setMode, changeAlt,
  } from '$lib/controllers/vehicleControl';

  let { telem, width, height, editing = false }: {
    telem: TelemetryData;
    width: number;
    height: number;
    editing?: boolean;
  } = $props();

  /** How long the last command's result stays on the card. */
  const FEEDBACK_MS = 6000;

  let alt = $state(20);
  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  const off = $derived(editing || !$controlAvailable || !$activeVehicle);
  const busy = $derived($busyAction !== null);
  const holdMode = $derived($activeVehicle ? resolveIntentMode($activeVehicle, 'hold') : undefined);
  const feedback = $derived($lastFeedback && now - $lastFeedback.ts < FEEDBACK_MS ? $lastFeedback : null);

  // Holding Land / Takeoff must not be the widget dock's long-press (→ edit mode): presses on the controls stay
  // here. The header still passes them on, so holding it rearranges the widget like any other.
  function keepPress(e: PointerEvent): void {
    if (!editing) e.stopPropagation();
  }

  async function armAndTakeoff(): Promise<void> {
    // ArduCopter disarms again after ~10 s on the ground without a takeoff: arm and take off in one go.
    if (!$isArmed && !(await arm())) return;
    await takeoff(alt);
  }
</script>

<div class="qa" style:--qw="{width}px" style:--qh="{height}px" class:off>
  <div class="head">
    <span class="title">{$t('widgets.quickActions')}</span>
    <span class="who">{$activeVehicle?.name ?? $t('quick.noVehicle')}</span>
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="grid" onpointerdown={keepPress}>
    <Button size="sm" variant="warning" full disabled={off || busy} onclick={() => void rtl()}>{$t('control.action.rtl')}</Button>
    <Button size="sm" full disabled={off || busy || !holdMode} onclick={() => holdMode && void setMode(holdMode)}>
      {holdMode ? $t(`control.mode.${holdMode.key}`, { default: holdMode.name }) : $t('quick.hold')}
    </Button>
    <HoldToConfirm variant="warning" duration={700} disabled={off} busy={$busyAction === 'land'} onconfirm={() => void land()}>
      {$t('control.action.land')}
    </HoldToConfirm>
    <HoldToConfirm duration={700} disabled={off || busy} busy={$busyAction === 'takeoff'} onconfirm={() => void armAndTakeoff()}>
      {$t('control.action.takeoff')}
    </HoldToConfirm>
    {#if $isArmed}
      <HoldToConfirm variant="danger" duration={1000} disabled={off} busy={$busyAction === 'disarm'} onconfirm={() => void disarm()}>
        {$t('quick.disarm')}
      </HoldToConfirm>
    {:else}
      <HoldToConfirm variant="danger" duration={1000} disabled={off || busy} busy={$busyAction === 'arm'} onconfirm={() => void arm()}>
        {$t('quick.arm')}
      </HoldToConfirm>
    {/if}
    <span class="cur">{$t('quick.altNow', { values: { alt: Math.round(telem.altitude) } })}</span>
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="alt" onpointerdown={keepPress}>
    <NumberStepper bind:value={alt} min={2} max={500} step={5} unit="m" disabled={off} />
    <Button size="sm" variant="data" disabled={off || busy || !$isArmed} title={$t('quick.goHint')} onclick={() => void changeAlt(alt)}>
      {$t('quick.go')}
    </Button>
  </div>
  <div class="fb" class:err={feedback && !feedback.ok}>
    {#if feedback}
      {feedback.ok ? '✓' : '✗'} {$t(`control.action.${feedback.action}`)}{feedback.message ? ` — ${feedback.message}` : ''}
    {/if}
  </div>
</div>

<style>
  .qa {
    width: var(--qw);
    height: var(--qh);
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 8px;
    background: rgba(30, 30, 30, 0.75);
    backdrop-filter: var(--glass-blur, blur(6px));
    -webkit-backdrop-filter: var(--glass-blur, blur(6px));
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    color: #ddd;
    overflow: hidden;
  }
  .qa.off { opacity: 0.6; }
  .head { display: flex; align-items: baseline; gap: 6px; min-width: 0; }
  .title { font-size: 11px; font-weight: 600; color: #37a8db; text-transform: uppercase; letter-spacing: 0.05em; }
  .who { font-size: 11px; color: #bbb; margin-left: auto; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 4px; }
  .cur { font-size: 11px; color: #aaa; align-self: center; text-align: center; }
  .alt { display: flex; align-items: center; gap: 6px; }
  .alt :global(> :first-child) { flex: 1; min-width: 0; }
  .fb { font-size: 10px; color: #8c8; min-height: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .fb.err { color: #e0a53c; }
</style>
