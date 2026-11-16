//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// Who really draws and shades a render, and the work the graphics card left to the processor.
// Pure functions of the state the server gives (`job`: `settings`, the engine's own word on what
// it took; `asked`, what the settings asked at launch; `fallbacks`, the last `fallback` event of
// each kind) and of the machine (`machine.cards`), used by the page (app.js). They do not touch
// the page: the texts come from ./i18n.js.

import { t, formatNumber } from './i18n.js';

const number = (n) => formatNumber(Math.round(n));

/**
 * The kinds of work the processor may do instead of the card, each with its own text
 * (`fallback.alert.<kind>`, one block or several). A block the chain on the card did not make
 * (`chain`) was either made again whole on the processor (this block only failed) or went the way
 * before the chain once it was given up (the card drawing and shading if it still could, the
 * processor for the rest): the event does not say which, its reason does.
 */
const KINDS = ['drawing', 'shading', 'chain'];

/** Whether a job is a render (a preview of a zone, or the whole map). */
export function isRender(job) {
  return Boolean(job) && (job.kind === 'zone' || job.kind === 'map');
}

/** Whether the machine has a card that can draw: a dedicated or integrated one, not the software one. */
export function hasUsableGpu(machine) {
  return Boolean(machine && Array.isArray(machine.cards) && machine.cards.some((c) => c && c.type !== 'software'));
}

/** Whether the settings of the render asked the card to draw: `gpu`, or `auto` with a usable card. */
export function gpuRequested(job, machine) {
  const asked = job && job.asked;
  if (!asked) return false;
  return asked.renderer === 'gpu' || (asked.renderer === 'auto' && hasUsableGpu(machine));
}

/** The fallbacks the run told, with at least one block: `[{kind, blocks, reason}]`. */
function fallbacksOf(job) {
  return (job && Array.isArray(job.fallbacks) ? job.fallbacks : []).filter((f) => f && typeof f.blocks === 'number' && f.blocks > 0);
}

/** "25 blocks drawn by the CPU — reason", "1 block shaded by the CPU — reason", for a kind of work. */
function alertText(kind, blocks, reason) {
  const params = { count: blocks, n: number(blocks), kind, reason };
  return KINDS.includes(kind) ? t(`fallback.alert.${kind}`, params) : t('fallback.alert.unknown', params);
}

/**
 * The alerts of a render, one per kind of work the card did not do: `[{kind, head, text}]`, empty
 * when the card did all it was asked. `head` is the part shown in bold, `text` the rest. Also from
 * the start, when the settings asked the card to draw (`gpu`, or `auto` with a usable card) and the
 * run says the processor draws.
 */
export function fallbackAlerts(job, machine) {
  if (!isRender(job)) return [];
  const alerts = fallbacksOf(job).map((f) => ({
    kind: f.kind,
    head: t('fallback.head'),
    text: alertText(f.kind, f.blocks, f.reason || t('fallback.reason.none')),
  }));
  const s = job.settings;
  if (s && s.renderer === 'cpu' && gpuRequested(job, machine) && !alerts.some((a) => a.kind === 'drawing')) {
    const reason = job.asked.renderer === 'gpu' ? t('fallback.reason.gpuNotOpened') : t('fallback.reason.gpuNotUsed');
    alerts.unshift({ kind: 'drawing', head: t('fallback.head'), text: t('fallback.alert.allDrawn', { reason }) });
  }
  return alerts;
}

/**
 * Who really drew, shaded and finished the blocks, in words: `{drawing, shading, chain}` (`chain`
 * null when the processor finished none in the card's place), or null before the run said what
 * it took.
 */
export function devices(job) {
  const s = job && job.settings;
  if (!s) return null;
  const fallbacks = fallbacksOf(job);
  const fallback = (kind) => fallbacks.find((f) => f.kind === kind);
  const withCpu = (device, kind) => {
    const f = fallback(kind);
    return f ? t('fallback.device.withCpu', { count: f.blocks, n: number(f.blocks), device }) : device;
  };
  const drawing = s.renderer === 'gpu'
    ? withCpu(s.card ? t('fallback.device.gpuNamed', { name: s.card }) : t('fallback.device.gpu'), 'drawing')
    : t('fallback.device.cpu');
  // the engine says `cpu` too when nothing is shaded: the light is off
  let shading;
  if (job.asked && job.asked.lighting === false) shading = t('fallback.device.lightingOff');
  else shading = s.shading === 'gpu' ? withCpu(t('fallback.device.gpu'), 'shading') : t('fallback.device.cpu');
  // (finished there, or made again whole: see KINDS)
  const c = fallback('chain');
  const chain = c ? t('fallback.device.chainTakenOver', { count: c.blocks, n: number(c.blocks) }) : null;
  return { drawing, shading, chain };
}

/** The line beside the gauges: "Drawing: graphics card … · Shading: graphics card", null before the run said. */
export function devicesLine(job) {
  const d = devices(job);
  if (!d) return null;
  return d.chain ? t('fallback.lineWithChain', d) : t('fallback.line', d);
}
