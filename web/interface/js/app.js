//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// The page of the interface: settings on the left, the map in the middle, the progress of the
// work on the right. It only talks to the program that serves it, through the routes of the
// contract (`/api/state`, `/api/settings`, `/api/launch`…), every request carrying the token the
// server wrote in the page. With `?demo=1` the same code talks to a fake server (demo.js).
//
// One loop asks `/api/state?since=<cursor>` (every 700 ms while a job runs, every second
// otherwise, for the gauges of the load; never two calls at once) and everything shown follows
// from the answer: a page reloaded in the middle of a render gets every event of the job again,
// mosaic included.
//
// Every text the page shows goes through `t('app.…')` (i18n.js), in the language of the settings
// (`settings.language`, or the browser's when it is not set); a change of language draws
// everything again.

import { createMap, BLOCK, SAN_ANDREAS } from './map.js';
import { fallbackAlerts, devices, devicesLine } from './fallback.js';
import { t, setLanguage, language, LANGUAGES, applyTranslations, formatNumber } from './i18n.js';

// ------------------------------------------------------------------------------------- small tools

const $ = (id) => document.getElementById(id);

/** Sets the text of an element, only when it changes. */
function setText(el, value) {
  if (el.textContent !== value) el.textContent = value;
}

function setHidden(el, hidden) {
  if (el.hidden !== hidden) el.hidden = hidden;
}

function h(tag, attributes, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attributes || {})) {
    if (key === 'class') el.className = value;
    else if (key.startsWith('on')) el.addEventListener(key.slice(2), value);
    else if (value !== false && value !== null && value !== undefined) el.setAttribute(key, value === true ? '' : value);
  }
  el.append(...children.filter((e) => e !== null && e !== undefined && e !== false));
  return el;
}

const ICONS = {
  menu: ['M2.5 4.5h11M2.5 8h11M2.5 11.5h11'],
  plus: ['M8 3v10M3 8h10'],
  minus: ['M3 8h10'],
  frame: ['M2.75 6V2.75H6M10 2.75h3.25V6M13.25 10v3.25H10M6 13.25H2.75V10'],
  close: ['M3.75 3.75l8.5 8.5M12.25 3.75l-8.5 8.5'],
  chevron: ['M4 6.25 8 10l4-3.75'],
};

/** A line icon on a 16 × 16 grid, in the colour of the text around it. */
function icon(name, size = 16) {
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  for (const [key, value] of Object.entries({
    viewBox: '0 0 16 16', width: size, height: size, fill: 'none', stroke: 'currentColor',
    'stroke-width': 1.5, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true', class: 'icon',
  })) svg.setAttribute(key, value);
  for (const d of ICONS[name]) {
    const stroke = document.createElementNS(NS, 'path');
    stroke.setAttribute('d', d);
    svg.append(stroke);
  }
  return svg;
}

/**
 * The icons the page adds to its buttons: added again when a translation replaced the text of
 * their button (and so the icon with it).
 */
const BUTTON_ICONS = [
  ['settings-toggle', 'menu', 16],
  ['zoom-in', 'plus', 16],
  ['zoom-out', 'minus', 16],
  ['zoom-all', 'frame', 16],
  ['tools-button', 'chevron', 14],
];

function addButtonIcons() {
  for (const [id, name, size] of BUTTON_ICONS) {
    const button = $(id);
    if (!button.querySelector(':scope > svg.icon')) button.append(icon(name, size));
  }
}

// Numbers, sizes and durations are written in the language of the page (Intl, through i18n.js).
const whole = (n) => formatNumber(Math.round(n));
const decimal = (x, digits) => formatNumber(x, { minimumFractionDigits: digits, maximumFractionDigits: digits });
/** A value with a unit Intl knows (`gigabyte`, `meter`, `kilometer`), with `digits` decimals. */
const unit = (x, name, digits) => formatNumber(x, { style: 'unit', unit: name, unitDisplay: 'short', minimumFractionDigits: digits, maximumFractionDigits: digits });
/** "42 %" for 42, as the language writes it. */
const percent = (v) => formatNumber(v / 100, { style: 'percent', minimumFractionDigits: 0, maximumFractionDigits: 0 });
/**
 * A count and its noun: the key's `.one` / `.other` (and the other plural forms of the language)
 * chosen by `count`; the text writes it as `{n}`, already formatted.
 */
const plural = (key, count, params) => t(key, { ...params, count, n: whole(count) });

/**
 * A translated sentence with elements in it: each `{name}` of `elements` stays a mark in the text,
 * then the text is cut at the marks and the elements put in their place. Returns the parts, for
 * `append` or `h`.
 */
function sentence(key, params, elements) {
  const marks = Object.fromEntries(Object.keys(elements).map((name) => [name, `{${name}}`]));
  return t(key, { ...params, ...marks })
    .split(/(\{\w+\})/)
    .filter((part) => part !== '')
    .map((part) => {
      const mark = /^\{(\w+)\}$/.exec(part);
      return mark && elements[mark[1]] ? elements[mark[1]] : part;
    });
}

/**
 * A message of the server: its own words in the language of the page when its `code` has a
 * translation (`server.<code>`), else the English text it sent.
 */
function serverText(text, code, params) {
  if (code) {
    const key = `server.${code}`;
    const translated = t(key, params || {});
    if (translated !== key) return translated;
  }
  return text || '';
}

/** A duration as it is read on a clock: "42 s", "3 min 05 s", "1 h 12 min". */
function duration(seconds) {
  const s = Math.max(Math.round(seconds), 0);
  if (s < 60) return t('app.duration.seconds', { s: whole(s) });
  if (s < 3600) return t('app.duration.minutesSeconds', { min: whole(Math.floor(s / 60)), s: String(s % 60).padStart(2, '0') });
  return t('app.duration.hoursMinutes', { h: whole(Math.floor(s / 3600)), min: String(Math.floor((s % 3600) / 60)).padStart(2, '0') });
}

/** An estimate, rounded like one: "20 s", "1 min", "12 min", "1 h 10 min". */
function roundDuration(seconds) {
  if (seconds < 50) return t('app.duration.seconds', { s: whole(Math.max(Math.ceil(seconds / 5) * 5, 5)) });
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t('app.duration.minutes', { min: whole(minutes) });
  return t('app.duration.hoursMinutes', { h: whole(Math.floor(minutes / 60)), min: String(minutes % 60).padStart(2, '0') });
}

const CARDINALS = ['north', 'northEast', 'east', 'southEast', 'south', 'southWest', 'west', 'northWest'];
const cardinal = (azimuth) => t(`app.compass.${CARDINALS[Math.round(azimuth / 45) % 8]}`);

const clone = (v) => JSON.parse(JSON.stringify(v));

function same(a, b) {
  if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) < 1e-4;
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((v, i) => v === b[i]);
  return a === b;
}

// Only conveniences are remembered by the browser: the view of the map, what is folded, and the
// resource folders that were unticked (the contract has no field for those). The language is a
// setting of the server: the port, and so the origin of the page, can change.
const STORAGE = {
  view: 'btms.view',
  sections: 'btms.sections',
  sidebar: 'btms.sidebar',
  resources: 'btms.resources',
};

function readStorage(key) {
  try {
    const raw = localStorage.getItem(key);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function writeStorage(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // private window, full or refused storage: nothing essential is lost
  }
}

// ------------------------------------------------------------------------------------- toasts, dialog

function toast(message, kind = 'info', ms = 7000) {
  const el = h('div', { class: `toast${kind === 'error' ? ' toast-error' : ''}`, role: kind === 'error' ? 'alert' : 'status' },
    h('span', { class: 'toast-text' }, message),
    h('button', { type: 'button', 'aria-label': t('app.toast.close'), onclick: () => el.remove() }, icon('close', 14)),
  );
  $('toasts').append(el);
  while ($('toasts').children.length > 4) $('toasts').firstElementChild.remove();
  if (ms) setTimeout(() => el.remove(), ms);
  return el;
}

/** Shows the server's sentence for a failed request (never a blocking alert). */
function report(error) {
  toast(error && error.message ? error.message : String(error), 'error', 10000);
}

/**
 * Asks for a confirmation in the page. `lines`: strings or elements. Resolves to true when the
 * user confirms.
 */
function confirmDialog({ title, lines, yes, no = t('app.dialog.cancel'), danger = false }) {
  const dialog = $('dialog');
  setText($('dialog-title'), title);
  $('dialog-body').replaceChildren(...lines.map((l) => (typeof l === 'string' ? h('p', null, l) : l)));
  setText($('dialog-yes'), yes);
  setText($('dialog-no'), no);
  $('dialog-yes').className = danger ? 'btn btn-danger' : 'btn btn-primary';
  // one question at a time: a second one while the first is open is a no
  if (answerDialog) return Promise.resolve(false);
  return new Promise((resolve) => {
    answerDialog = (agreed) => {
      answerDialog = null;
      if (dialog.open) dialog.close();
      resolve(agreed);
    };
    dialog.showModal();
    $('dialog-no').focus();
  });
}

/** Answers the open confirmation (see `confirmDialog`), or null when none is open. */
let answerDialog = null;

// ------------------------------------------------------------------------------------- transport

const query = new URLSearchParams(location.search);
const tokenMeta = document.querySelector('meta[name="btms-token"]');
const token = tokenMeta ? tokenMeta.content : '';

class ApiError extends Error {
  /**
   * `english`, `code` and `params`: what the server said, to write it again in another language
   * (`server.<code>`).
   */
  constructor(status, message, code = null, params = null, english = null) {
    super(message);
    this.status = status;
    this.code = code;
    this.params = params;
    this.english = english;
  }

  /** The message in the language of the page now. */
  text() {
    return this.code ? serverText(this.english, this.code, this.params) : this.message;
  }
}

/** The real transport: resolves to { status, json }, rejects when the server cannot be reached. */
async function realRequest(method, path, headers, body) {
  const options = { method, headers: { ...headers }, cache: 'no-store' };
  if (body !== undefined) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(body);
  }
  // the file dialog stays open as long as the user wants; anything else answers at once
  if (path !== '/api/choose' && typeof AbortSignal.timeout === 'function') options.signal = AbortSignal.timeout(20000);
  const response = await fetch(path, options);
  let json = null;
  try {
    json = await response.json();
  } catch {
    json = null;
  }
  return { status: response.status, json };
}

let transport = realRequest;

/** One request to the program, with the token. Throws ApiError (status 0: no answer). */
async function api(method, path, body) {
  let response;
  try {
    response = await transport(method, path, { 'X-BTMS-Token': token }, body);
  } catch {
    throw new ApiError(0, t('app.api.noAnswer'));
  }
  if (response.status >= 400) {
    const json = response.json;
    if (json && typeof json.error === 'string') {
      throw new ApiError(response.status, serverText(json.error, json.code, json.params), json.code || null, json.params || null, json.error);
    }
    throw new ApiError(response.status, t('app.api.errorStatus', { status: String(response.status) }));
  }
  return response.json || {};
}

// ------------------------------------------------------------------------------------- state

/** The last State the server gave (its `job` only ever comes from the polling loop). */
let server = null;
let receivedAt = 0;
let cursor = null;
/** Id of the job whose events are shown (undefined before the first answer). */
let jobId;
let inFlight = false;
let pollAgain = false;
let pollTimer = 0;
let stopped = false;
let connected = true;

/** What the controls show: the server's settings, plus what was changed here and not sent yet. */
let settings = null;
const touched = new Set();
let sending = null;
let settingsTimer = 0;

/** Folder and revision of each source of tiles, as last seen. */
const known = {};
let shownSource = 'map';
let zone = null;
let planKnown = false;
let fitToPlan = false;

const log = { lines: [], shown: 0, cut: false };
let blockErrors = [];
/** Blocks per second of the last render seen, for the estimates. */
let knownSpeed = null;
let choosing = false;
let demo = null;
let map = null;

const jobRunning = () => Boolean(server && server.job && server.job.status === 'running');
const isRender = (job) => Boolean(job) && (job.kind === 'zone' || job.kind === 'map' || job.kind === 'update');
/** Jobs whose result is the list of "Resource changes", not the log. */
const isList = (job) => Boolean(job) && (job.kind === 'list-changes' || job.kind === 'record');

// ------------------------------------------------------------------------------------- language

/** The language of "auto": the first of the browser's languages that BTMS has, else English. */
function autoLanguage() {
  const tags = navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language];
  for (const tag of tags) {
    const code = String(tag || '').slice(0, 2).toLowerCase();
    if (Object.prototype.hasOwnProperty.call(LANGUAGES, code)) return code;
  }
  return 'en';
}

/** The language last asked for, and the changes of language, one after the other. */
let wantedLanguage = null;
let languageChanges = Promise.resolve();

/**
 * Shows the page in `code` (null or unknown: auto). Nothing happens when it is already the one
 * asked for; else, once its words are there, everything is drawn again in it.
 */
function useLanguage(code) {
  const wanted = code && Object.prototype.hasOwnProperty.call(LANGUAGES, code) ? code : autoLanguage();
  if (wanted === wantedLanguage) return;
  wantedLanguage = wanted;
  languageChanges = languageChanges
    .then(async () => {
      // a later choice came first: that one is drawn instead
      if (wantedLanguage !== wanted) return;
      await setLanguage(wanted);
      if (wantedLanguage !== wanted) return;
      redrawAll();
    })
    .catch(report);
}

/** The language the settings ask for (what was changed here and not sent yet comes first). */
function settingsLanguage() {
  if (settings) return settings.language ?? null;
  return server && server.settings ? server.settings.language ?? null : null;
}

function renderLanguageSelect() {
  const select = $('language-select');
  if (!select.options.length) {
    select.replaceChildren(...Object.entries(LANGUAGES).map(([code, name]) => h('option', { value: code, lang: code }, name)));
  }
  // the language asked for, even while its words are still loading
  const current = wantedLanguage || language();
  if (select.value !== current) select.value = current;
}

/**
 * Everything again in the language just chosen: the static text of the page, then what this file
 * writes (after it, so that it wins), including the parts built once and kept.
 */
function redrawAll() {
  applyTranslations(document);
  addButtonIcons();
  for (const el of document.querySelectorAll('[data-signature]')) delete el.dataset.signature;
  renderLanguageSelect();
  renderSettingsStatus();
  renderLogExpand();
  renderBlockErrors();
  if (log.cut) renderLog();
  onHover(lastHover);
  renderScale();
  render();
  if (!server) document.title = t('app.documentTitle.idle');
}

// ------------------------------------------------------------------------------------- polling

async function poll() {
  clearTimeout(pollTimer);
  if (stopped) return;
  if (inFlight) {
    pollAgain = true;
    return;
  }
  inFlight = true;
  const incremental = cursor !== null;
  try {
    const state = await api('GET', incremental ? `/api/state?since=${cursor}` : '/api/state');
    setConnected(true);
    receive(state, incremental);
  } catch (error) {
    if (error.status === 403) staleSession();
    else if (error.status === 0) setConnected(false);
    else if (error instanceof ApiError) reportOnce(error);
    else throw error;
  } finally {
    inFlight = false;
    if (!stopped) {
      const delay = pollAgain ? 0 : !connected ? 1500 : jobRunning() ? 700 : 1000;
      pollAgain = false;
      pollTimer = setTimeout(poll, delay);
    }
  }
}

let lastError = '';
function reportOnce(error) {
  if (error.message === lastError) return;
  lastError = error.message;
  report(error);
}

function setConnected(up) {
  if (connected === up) return;
  connected = up;
  setHidden($('connection-banner'), up);
  $('app').classList.toggle('offline', !up);
  if (up) {
    lastError = '';
    // tiles that failed while the server was away are asked again, and so are the pending settings
    map.retryMissing();
    if (touched.size) scheduleSend(0);
  }
  renderActions();
}

/** The server no longer knows this page's token: it was restarted. One reload gets the new one. */
function staleSession() {
  stopped = true;
  let recent = false;
  try {
    recent = Date.now() - Number(sessionStorage.getItem('btms.reload') || 0) < 15000;
    if (!recent) sessionStorage.setItem('btms.reload', String(Date.now()));
  } catch {
    recent = true;
  }
  if (!recent) {
    location.reload();
    return;
  }
  const banner = $('connection-banner');
  banner.replaceChildren(h('span', null, h('b', null, t('app.session.staleHead')), ' ', t('app.session.staleBody')));
  setHidden(banner, false);
  $('app').classList.add('offline');
}

function receive(state, incremental) {
  const id = state.job ? state.job.id : null;
  // events carry no job id: when the job changed (or the server restarted), what came with
  // `since` cannot be told apart. Everything of the shown job is asked again instead.
  if (incremental && (id !== jobId || state.cursor < cursor)) {
    cursor = null;
    pollAgain = true;
    return;
  }
  const first = server === null;
  const isNew = !incremental && id !== jobId;
  const before = server ? server.job : null;
  server = state;
  receivedAt = performance.now();
  cursor = state.cursor;
  if (!incremental) {
    clearProgress();
    jobId = id;
  }
  if (isNew && state.job) onNewJob(state.job, first);
  noteChanges(state.job);
  applyEvents(state.events || [], !incremental);
  followSources(state.sources || {}, state.job);
  adoptSettings(state.settings);
  const job = state.job;
  if (job && job.status !== 'running') {
    map.freezePlan();
    if (before && before.id === job.id && before.status === 'running') onJobEnd(job);
  }
  if (job && typeof job.speed === 'number' && job.speed > 0) knownSpeed = job.speed;
  if (first) firstView();
  render();
}

function clearProgress() {
  map.setPlan(null);
  planKnown = false;
  log.lines = [];
  log.shown = 0;
  log.cut = false;
  $('log-lines').replaceChildren();
  blockErrors = [];
  $('errors-list').replaceChildren();
}

/** A job this page has not shown yet: started from here, from another window, or before it opened. */
function onNewJob(job, first) {
  const running = job.status === 'running';
  if (isRender(job)) {
    if (job.zone && (running || (first && !zone))) map.setZone(job.zone);
    if (running || !first) {
      showSource(job.source || 'map');
      // the preview folder was emptied, the map folder is being rewritten
      if (running) map.invalidate(shownSource);
      // its progress is at the top of the column (an update may be launched from the list, below)
      if (running) document.querySelector('.progress-body').scrollTop = 0;
      if (job.zone) map.fitBounds(job.zone, { margins: mapMargins(60, true) });
      else fitToPlan = running;
    }
  } else if (isList(job)) {
    if (running || !first) $('changes').open = true;
  } else if (running || !first) {
    $('log').open = true;
  }
}

function onJobEnd(job) {
  if (!isRender(job)) return;
  if (job.source) map.invalidate(job.source);
  if (!knownSpeed && job.blocks_done >= 4 && job.duration_ms > 0) knownSpeed = job.blocks_done / (job.duration_ms / 1000);
}

function applyEvents(events, replayed) {
  const src = server.job ? server.job.source : null;
  const lines = [];
  for (const ev of events) {
    if (ev.t === 'plan') {
      map.setPlan(ev.blocks, src);
      planKnown = Boolean(ev.blocks && ev.blocks.length);
      if (fitToPlan && planKnown) {
        map.fitBounds(map.planBounds(), { margins: mapMargins(40, true) });
        fitToPlan = false;
      }
    } else if (ev.t === 'block') {
      map.markBlock(ev.x, ev.y, ev.state, ev.message, replayed);
      if (ev.state === 'done' && !replayed && src) map.invalidateBlock(src, ev.x, ev.y);
      if (ev.state === 'failed') addBlockError(ev);
    } else if (ev.t === 'zooms') {
      // replayed ones too: the blocks finished after the last pass are the ones not yet in the far zooms
      if (src) map.invalidateFarZooms(src);
    } else if (ev.t === 'log') {
      lines.push(String(ev.text));
    }
  }
  if (lines.length) appendLog(lines);
}

/**
 * What the tiles of each source became. During a render the events say which tiles changed; any
 * other change of folder or revision (a preview folder emptied, another folder chosen, a render
 * made while the page was closed) asks the whole source again.
 */
function followSources(sources, job) {
  for (const name of ['map', 'preview']) {
    const s = sources[name];
    if (!s) continue;
    map.setRevision(name, s.rev);
    const k = known[name];
    const rendering = Boolean(job) && job.status === 'running' && job.source === name;
    if (k && (k.folder !== s.folder || (k.rev !== s.rev && !rendering))) map.invalidate(name);
    known[name] = { folder: s.folder, rev: s.rev };
    // No tile is asked where the source cannot have one: nowhere without a manifest, outside the
    // zone while a preview is rendered into it (its folder was emptied), else outside what its
    // tiles cover. Unknown while the whole map is rendered: the tiles of the render before are
    // still there. While it is updated: what its tiles cover, and the ground of the update (new
    // tiles may come beyond them).
    let extent = null;
    if (rendering && job.kind === 'zone') extent = job.zone ? wholeBlocks(job.zone) : null;
    else if (rendering && job.kind === 'update') extent = s.exists && s.bounds ? union(s.bounds, job.zone ? wholeBlocks(job.zone) : map.planBounds()) : null;
    else extent = s.exists ? s.bounds : NO_TILE;
    map.setExtent(name, extent);
  }
}

/** The ground of a source that has no tile at all: nothing meets it. */
const NO_TILE = [0, 0, 0, 0];

/** The smallest rectangle holding two (the first alone when the second is null). */
function union(a, b) {
  if (!b) return a;
  return [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.max(a[2], b[2]), Math.max(a[3], b[3])];
}

/** A zone widened to whole blocks: what its tiles cover. */
function wholeBlocks(z) {
  return [Math.floor(z[0] / BLOCK) * BLOCK, Math.floor(z[1] / BLOCK) * BLOCK, Math.ceil(z[2] / BLOCK) * BLOCK, Math.ceil(z[3] / BLOCK) * BLOCK];
}

// ------------------------------------------------------------------------------------- settings: state

const SECTIONS = {
  lighting: ['enabled', 'azimuth', 'elevation', 'strength', 'shadows', 'ambient_occlusion'],
  maps: ['keep', 'exclude_folders', 'fill_holes'],
  resources: ['folders'],
  water: ['enabled'],
};

function adoptSettings(received) {
  if (touched.size || sending || !received) return;
  settings = clone(received);
  if (!settings.overrides || typeof settings.overrides !== 'object') settings.overrides = {};
}

/** The override of a field, or undefined (absent and null both mean "as in map.toml"). */
function override(section, field) {
  const s = settings && settings.overrides ? settings.overrides[section] : null;
  const v = s ? s[field] : undefined;
  return v === null ? undefined : v;
}

const fromBase = (section, field) => (server.base[section] || {})[field];

/**
 * What a render started from here will use for a field: what the server says (`effective`), or,
 * while a change made here is not sent yet, map.toml with the overrides of the page.
 */
function value(section, field) {
  const said = server.effective && server.effective[section];
  if (!touched.size && !sending && said && said[field] !== undefined && said[field] !== null) return said[field];
  const v = override(section, field);
  return v !== undefined ? v : fromBase(section, field);
}

/** Sets a field; a value equal to map.toml's removes the override. */
function setField(section, field, v, delay = 350) {
  const s = settings.overrides;
  if (!s[section] || typeof s[section] !== 'object') s[section] = {};
  if (same(v, fromBase(section, field))) delete s[section][field];
  else s[section][field] = v;
  touch('overrides', delay);
}

function touch(key, delay = 350) {
  touched.add(key);
  renderSettings();
  renderActions();
  scheduleSend(delay);
}

function scheduleSend(delay) {
  clearTimeout(settingsTimer);
  settingsTimer = setTimeout(sendSettings, delay);
}

/**
 * The overrides in full: every field, null for "as in map.toml". Whatever the depth at which
 * the server merges, what it keeps is exactly this.
 */
function fullOverrides() {
  const full = {};
  for (const [section, fields] of Object.entries(SECTIONS)) {
    full[section] = {};
    for (const field of fields) {
      const v = override(section, field);
      full[section][field] = v === undefined ? null : v;
    }
  }
  return full;
}

/** Sends what changed (after a short delay: not one request per notch of a slider). */
async function sendSettings() {
  clearTimeout(settingsTimer);
  while (sending) await sending;
  if (!touched.size || !settings) return;
  const keys = [...touched];
  const body = {};
  for (const key of keys) body[key] = key === 'overrides' ? fullOverrides() : settings[key];
  touched.clear();
  const promise = (async () => {
    try {
      const state = await api('POST', '/api/settings', body);
      // settings, checks and sources of the answer; the job and its events stay the loop's
      if (server && state.settings) {
        server = { ...server, ...state, job: server.job, cursor: server.cursor, events: [] };
        followSources(state.sources || {}, server.job);
      }
      setSettingsStatus(() => t('app.settings.saved'), false);
    } catch (error) {
      if (error.status === 0) {
        // not sent: kept for when the program answers again
        for (const key of keys) touched.add(key);
        setSettingsStatus(() => t('app.settings.notSaved'), true);
      } else {
        // refused: the controls go back to what the program has
        setSettingsStatus(() => t('app.settings.refused', { message: error instanceof ApiError ? error.text() : error.message }), true);
        report(error);
      }
    }
  })();
  sending = promise;
  await promise;
  sending = null;
  if (server) adoptSettings(server.settings);
  render();
}

/** The status line under the settings: how to write it (again in another language), and whether it is an error. */
let settingsStatus = null;

function setSettingsStatus(write, error) {
  settingsStatus = { write, error };
  renderSettingsStatus();
}

function renderSettingsStatus() {
  if (!settingsStatus) return;
  const el = $('settings-status');
  setText(el, settingsStatus.write());
  el.classList.toggle('is-error', settingsStatus.error);
}

// ------------------------------------------------------------------------------------- settings: rendering

function renderSettings() {
  if (!server || !settings) return;
  renderPerformance();
  renderLighting();
  renderContent();
  renderOutput();
}

function setValue(el, v) {
  const s = String(v);
  if (el.value !== s) el.value = s;
}

/** The names of the lighting fields, as the "differs from map.toml" line says them. */
const LIGHTING_FIELD_KEYS = {
  enabled: 'app.lighting.field.enabled',
  azimuth: 'app.lighting.field.azimuth',
  elevation: 'app.lighting.field.elevation',
  strength: 'app.lighting.field.strength',
  shadows: 'app.lighting.field.shadows',
  ambient_occlusion: 'app.lighting.field.ambientOcclusion',
};

function renderLighting() {
  const enabled = value('lighting', 'enabled');
  const azimuth = value('lighting', 'azimuth');
  const elevation = value('lighting', 'elevation');
  const strength = value('lighting', 'strength');
  const occlusion = value('lighting', 'ambient_occlusion');
  $('lighting-enabled').checked = enabled;
  $('lighting-shadows').checked = value('lighting', 'shadows');
  $('lighting-controls').classList.toggle('is-inactive', !enabled);
  for (const el of $('lighting-controls').querySelectorAll('input, button')) el.disabled = !enabled;
  setValue($('lighting-azimuth'), azimuth);
  setValue($('lighting-elevation'), elevation);
  setValue($('lighting-strength'), strength);
  setValue($('lighting-ambient-occlusion'), occlusion);
  setText($('lighting-azimuth-value'), t('app.lighting.azimuthValue', { degrees: decimal(azimuth, 0), direction: cardinal(azimuth) }));
  setText($('lighting-elevation-value'), t('app.lighting.degrees', { degrees: decimal(elevation, 0) }));
  setText($('lighting-strength-value'), decimal(strength, 2));
  setText($('lighting-ambient-occlusion-value'), decimal(occlusion, 2));
  $('compass-sun').style.transform = `rotate(${azimuth}deg)`;
  for (const button of document.querySelectorAll('[data-lighting-preset]')) {
    const [pe, ps] = button.dataset.lightingPreset === 'low' ? [35, 0.6] : [45, 0.5];
    button.setAttribute('aria-pressed', String(same(elevation, pe) && same(strength, ps)));
  }

  const say = (field, v) => (typeof v === 'boolean'
    ? t(v ? 'app.lighting.on' : 'app.lighting.off')
    : field === 'azimuth' || field === 'elevation' ? t('app.lighting.degrees', { degrees: decimal(v, 0) }) : decimal(v, 2));
  const diffs = SECTIONS.lighting
    .filter((field) => !same(value('lighting', field), fromBase('lighting', field)))
    .map((field) => t('app.lighting.diff', { name: t(LIGHTING_FIELD_KEYS[field]), value: say(field, value('lighting', field)), base: say(field, fromBase('lighting', field)) }));
  renderDiff('lighting-diff', diffs.length ? t('app.diff.differs', { diffs: diffs.join(', ') }) : '');
  const note = $('lighting-note');
  setText(note, enabled ? t('app.lighting.note', { direction: cardinal(azimuth), elevation: decimal(elevation, 0) }) : t('app.lighting.noteOff'));
  note.classList.toggle('is-modified', diffs.length > 0);
}

function renderDiff(id, message) {
  const el = $(id);
  setHidden(el, !message);
  setText(el.querySelector('.diff-text'), message);
}

/**
 * How a list differs from map.toml's: "2 added, 1 removed", or '' when it is the same. `noun`:
 * what the list holds (`entries`, `folders`), for the languages whose words agree with it.
 */
function listDiff(section, field, noun) {
  const base = fromBase(section, field);
  const current = value(section, field);
  if (same(base, current)) return '';
  const added = current.filter((v) => !base.includes(v)).length;
  const removed = base.filter((v) => !current.includes(v)).length;
  const parts = [];
  if (added) parts.push(plural(`app.diff.${noun}.added`, added));
  if (removed) parts.push(plural(`app.diff.${noun}.removed`, removed));
  return t('app.diff.differs', { diffs: parts.length ? parts.join(', ') : t('app.diff.orderChanged') });
}

/** The lines of a text area: trimmed, no empty line, no line twice. */
function linesOf(rawText) {
  const seen = new Set();
  const lines = [];
  for (const raw of rawText.split(/\r?\n/)) {
    const line = raw.trim().replace(/^["']|["'],?$/g, '');
    if (!line || seen.has(line)) continue;
    seen.add(line);
    lines.push(line);
  }
  return lines;
}

/** Fills a text area with a list, unless the user is typing in it. */
function setList(el, list) {
  if (document.activeElement === el) return;
  const s = list.join('\n');
  if (el.value !== s) el.value = s;
}

function renderContent() {
  const keep = value('maps', 'keep');
  const exclude = value('maps', 'exclude_folders');
  setList($('maps-keep'), keep);
  setList($('maps-exclude-folders'), exclude);
  setText($('maps-keep-count'), plural('app.content.entries', keep.length));
  setText($('maps-exclude-folders-count'), plural('app.content.folders', exclude.length));
  const keepDiff = listDiff('maps', 'keep', 'entries');
  const excludeDiff = listDiff('maps', 'exclude_folders', 'folders');
  const resourcesDiff = listDiff('resources', 'folders', 'folders');
  renderDiff('maps-keep-diff', keepDiff);
  renderDiff('maps-exclude-folders-diff', excludeDiff);
  renderDiff('resources-folders-diff', resourcesDiff);

  // every folder known: those of map.toml, those added here, those unticked and remembered
  const active = value('resources', 'folders');
  const all = knownFolders();
  setText($('resources-folders-count'), t('app.content.folderCount', { active: whole(active.length), total: whole(all.length) }));
  const list = $('resources-folders');
  const signature = JSON.stringify([all, active]);
  if (list.dataset.signature !== signature) {
    list.dataset.signature = signature;
    const base = fromBase('resources', 'folders');
    list.replaceChildren(
      ...all.map((folder) => {
        const ticked = active.includes(folder);
        return h('li', { class: `folder${ticked ? '' : ' is-unticked'}` },
          h('input', { type: 'checkbox', checked: ticked, 'aria-label': t('app.content.draw', { folder }), onchange: (ev) => tickResource(folder, ev.target.checked) }),
          h('span', { class: 'path mono selectable', title: folder }, folder),
          !base.includes(folder) && h('button', { class: 'folder-remove', type: 'button', title: t('app.content.forgetTitle'), 'aria-label': t('app.content.forget', { folder }), onclick: () => forgetResource(folder) }, icon('close', 12)),
        );
      }),
    );
    if (!all.length) list.append(h('li', { class: 'folder-list-empty' }, t('app.content.noFolder')));
  }

  setText($('config-path'), settings.config || '');
  const { water, waterDiff, holesDiff } = renderWaterAndHoles();
  const modified = Boolean(keepDiff || excludeDiff || resourcesDiff || waterDiff || holesDiff);
  const note = $('content-note');
  setText(note, t(water ? 'app.content.note' : 'app.content.noteNoWater', { maps: plural('app.content.maps', keep.length), resources: plural('app.content.resources', active.length) }));
  note.classList.toggle('is-modified', modified);
}

/** The two switches of the water and of the holes of the ground, with their "differs from map.toml". */
function renderWaterAndHoles() {
  const water = value('water', 'enabled') !== false;
  const holes = value('maps', 'fill_holes') !== false;
  $('water-enabled').checked = water;
  $('maps-fill-holes').checked = holes;
  const baseWater = fromBase('water', 'enabled') !== false;
  const baseHoles = fromBase('maps', 'fill_holes') !== false;
  const drawn = (v) => t(v ? 'app.content.water.drawn' : 'app.content.water.notDrawn');
  const filled = (v) => t(v ? 'app.content.holes.filled' : 'app.content.holes.left');
  const waterDiff = water === baseWater ? '' : t('app.content.water.diff', { value: drawn(water), base: drawn(baseWater) });
  const holesDiff = holes === baseHoles ? '' : t('app.content.holes.diff', { value: filled(holes), base: filled(baseHoles) });
  renderDiff('water-diff', waterDiff);
  renderDiff('maps-fill-holes-diff', holesDiff);
  return { water, waterDiff, holesDiff };
}

/** The resource folders in a steady order: map.toml's, then the added ones, then the remembered. */
function knownFolders() {
  const all = [...fromBase('resources', 'folders')];
  for (const d of value('resources', 'folders')) if (!all.includes(d)) all.push(d);
  for (const d of readStorage(STORAGE.resources) || []) if (typeof d === 'string' && !all.includes(d)) all.push(d);
  return all;
}

function tickResource(folder, ticked) {
  const active = value('resources', 'folders');
  const all = knownFolders();
  // an unticked folder is no longer in the settings: the browser remembers it
  writeStorage(STORAGE.resources, all.filter((d) => !fromBase('resources', 'folders').includes(d)));
  setField('resources', 'folders', all.filter((d) => (d === folder ? ticked : active.includes(d))));
}

function forgetResource(folder) {
  writeStorage(STORAGE.resources, (readStorage(STORAGE.resources) || []).filter((d) => d !== folder));
  setField('resources', 'folders', value('resources', 'folders').filter((d) => d !== folder));
}

async function addResource() {
  const path = await choose('folder', t('app.choose.resources'), undefined, 'resources');
  if (!path) return;
  const active = value('resources', 'folders');
  if (active.includes(path)) {
    toast(t('app.content.alreadyListed'));
    return;
  }
  writeStorage(STORAGE.resources, [...knownFolders().filter((d) => !fromBase('resources', 'folders').includes(d)), path]);
  const all = knownFolders();
  setField('resources', 'folders', all.filter((d) => d === path || active.includes(d)), 0);
}

/** The quality of the WebP tiles when none is set (`DEFAULT_QUALITY` of the engine, tiles/format.rs). */
const DEFAULT_QUALITY = 90;

function renderOutput() {
  setText($('output-path'), settings.output || '');
  setText($('preview-path'), settings.preview || '');
  for (const button of document.querySelectorAll('[data-change]')) button.disabled = jobRunning();

  // what the tiles are written as: WebP (the engine's default) at a quality, or the older format
  const format = settings.format === 'jpeg' ? 'jpeg' : 'webp';
  renderChoice('format-choice', 'format', [
    ['webp', t('app.output.webp'), t('app.output.webpDetail')],
    ['jpeg', t('app.output.jpeg'), t('app.output.jpegDetail')],
  ]);
  for (const b of $('format-choice').children) b.setAttribute('aria-pressed', String(b.dataset.value === format));
  const q = settings.quality === null || settings.quality === undefined ? DEFAULT_QUALITY : settings.quality;
  const lossless = q === 'lossless';
  const jpeg = format === 'jpeg';
  const slider = $('quality');
  setValue(slider, lossless ? 100 : Number(q));
  slider.disabled = jpeg || lossless;
  $('lossless').checked = lossless;
  $('lossless').disabled = jpeg;
  document.querySelector('[data-default="quality"]').disabled = jpeg || q === DEFAULT_QUALITY;
  setText($('quality-value'), jpeg ? '—' : lossless ? t('app.output.lossless') : q === DEFAULT_QUALITY ? t('app.output.qualityDefault', { quality: whole(q) }) : whole(Number(q)));
  setText($('quality-help'), jpeg
    ? t('app.output.help.jpeg')
    : lossless
      ? t('app.output.help.lossless')
      : q === DEFAULT_QUALITY ? t('app.output.help.default', { quality: whole(DEFAULT_QUALITY) }) : q < DEFAULT_QUALITY ? t('app.output.help.lower') : t('app.output.help.higher'));
  const folder = (settings.output || '').split(/[\\/]/).filter(Boolean).pop() || '';
  // (the format first: a long folder name is the part cut short)
  setText($('output-note'), [jpeg ? t('app.output.note.jpeg') : lossless ? t('app.output.note.lossless') : t('app.output.note.webp', { quality: whole(Number(q)) }), folder].filter(Boolean).join(' · '));
}

// ------------------------------------------------------------------------------------- settings: performance

/** The engine's budgets when none is given, in GB (`--ram`, `--vram`). */
const DEFAULT_RAM = 6;
const DEFAULT_VRAM = 4;
const PERF_FIELDS = ['threads', 'renderer', 'shading', 'ram_gb', 'vram_gb', 'priority'];
/** The presets, in the order they are recognised (their names and helps are `app.perf.preset.<name>`). */
const PRESETS = ['maximum', 'balanced', 'quiet'];
/** The settings when nothing was set: the engine's own values. */
const PERF_DEFAULTS = { threads: 'auto', renderer: 'auto', shading: 'auto', ram_gb: null, vram_gb: null, priority: 'normal' };
/**
 * What a render holds on the card beside its textures, in GB: per drawing thread (its pictures,
 * about 0.11 GB for a block and its heights, and their geometry), the shading's (8 sets), and a
 * margin for the system and the other programs at rest (3.5 GB measured on a test machine, less
 * the threads' own slack) and for the textures sent between two pictures (the engine keeps to its
 * budget when a picture begins).
 */
const CARD_PER_THREAD = 0.2;
const CARD_SHADING = 0.6;
const CARD_MARGIN = 3;

const machine = () => (server && server.machine) || null;
const logicalCores = () => Math.max((machine() && machine().cores) || (server && server.cores) || 1, 1);
const halfGb = (x) => Math.floor(x * 2) / 2;
/** "6 GB", "17.5 GB". */
const gb = (x) => unit(x, 'gigabyte', Number.isInteger(x) ? 0 : 1);

/**
 * The card a render would draw on: a dedicated one first, the one with the most memory first;
 * never a software one. The server reads the load of the same one.
 */
function mainCard() {
  const m = machine();
  if (!m || !Array.isArray(m.cards)) return null;
  const rank = (c) => (c.type === 'dedicated' ? 0 : 1);
  const cards = m.cards.filter((c) => c.type !== 'software');
  cards.sort((a, b) => rank(a) - rank(b) || (b.memory_gb || 0) - (a.memory_gb || 0));
  return cards[0] || null;
}

/** Whether a render with settings `r` draws on `card`, and shades on it (the engine's `auto`). */
function onCard(r, card) {
  const drawing = Boolean(card) && r.renderer !== 'cpu';
  return { drawing, shading: Boolean(card) && (r.shading === 'gpu' || (r.shading !== 'cpu' && drawing)) };
}

/**
 * The drawing threads of a render with settings `r`, `auto` as the engine resolves it
 * (`tiles::options::threads`): one per logical core; without the chain on the card (`--chain
 * auto`: it is used when the card draws, and shades or the light is off), half as many again when
 * the card draws.
 */
function drawingThreads(r, card) {
  const n = logicalCores();
  if (typeof r.threads === 'number') return r.threads;
  const on = onCard(r, card);
  const chain = on.drawing && (on.shading || (server !== null && value('lighting', 'enabled') === false));
  return on.drawing && !chain ? Math.min(n + Math.floor(n / 2), 256) : Math.min(n, 256);
}

/**
 * What the card leaves to the textures of a render with settings `r`, in GB (half-GB steps, 0.5
 * at least): its memory less what the render holds on it beside them and the margin. Null without
 * a card of known memory.
 */
function textureRoom(r, card) {
  if (!card || !card.memory_gb) return null;
  const on = onCard(r, card);
  const left = card.memory_gb - (on.drawing ? drawingThreads(r, card) * CARD_PER_THREAD : 0) - (on.shading ? CARD_SHADING : 0) - CARD_MARGIN;
  return Math.max(halfGb(left), 0.5);
}

/** The three presets, from the machine (Maximum waits for it). */
function presets() {
  const m = machine();
  const n = logicalCores();
  const card = mainCard();
  const onGpu = card ? 'gpu' : 'auto';
  // Maximum's textures: half of the card, less when its threads and the shading leave less room
  // (a pure function of the machine: the preset stays recognised whatever the load does)
  const room = textureRoom({ threads: 'auto', renderer: onGpu, shading: onGpu }, card);
  return {
    maximum: m
      ? {
        threads: 'auto',
        renderer: onGpu,
        shading: onGpu,
        ram_gb: m.memory_gb ? Math.max(halfGb(m.memory_gb / 2), 1) : null,
        vram_gb: room === null ? null : Math.max(Math.min(halfGb(card.memory_gb / 2), room), 0.5),
        priority: 'normal',
      }
      : null,
    balanced: { threads: Math.max(n - 2, 1), renderer: 'auto', shading: 'auto', ram_gb: null, vram_gb: null, priority: 'normal' },
    quiet: { threads: Math.max(Math.floor(n / 2), 1), renderer: 'auto', shading: 'cpu', ram_gb: null, vram_gb: null, priority: 'low' },
  };
}

function samePerformance(p) {
  return PERF_FIELDS.every((field) => {
    const a = settings[field] === undefined ? null : settings[field];
    const b = p[field] === undefined ? null : p[field];
    return a === null || b === null ? a === b : same(a, b);
  });
}

/** The preset the settings are, or null ("Custom"). */
function activePreset() {
  const p = presets();
  return PRESETS.find((key) => p[key] && samePerformance(p[key])) || null;
}

function applyPreset(key) {
  const p = presets()[key];
  if (!p || !settings) return;
  for (const field of PERF_FIELDS) {
    settings[field] = p[field];
    touched.add(field);
  }
  renderSettings();
  renderActions();
  scheduleSend(0);
}

function setSetting(field, v, delay = 0) {
  if (!settings) return;
  settings[field] = v;
  touch(field, delay);
}

/** A choice among a few (who draws, who shades): built once per set of options, pressed as the setting. */
function renderChoice(id, field, options) {
  const host = $(id);
  const signature = JSON.stringify(options);
  if (host.dataset.signature !== signature) {
    host.dataset.signature = signature;
    host.replaceChildren(
      ...options.map(([v, title, detail, refused]) =>
        h('button', { type: 'button', 'data-value': v, disabled: Boolean(refused), onclick: () => setSetting(field, v) },
          h('span', { class: 'choice-title' }, title),
          detail && h('span', { class: 'choice-detail' }, detail),
        ),
      ),
    );
  }
  for (const b of host.children) b.setAttribute('aria-pressed', String(b.dataset.value === (settings[field] || 'auto')));
}

function renderPerformance() {
  const n = logicalCores();
  const m = machine();
  const card = mainCard();
  const p = presets();
  const active = activePreset();
  for (const b of document.querySelectorAll('[data-perf-preset]')) {
    b.setAttribute('aria-pressed', String(b.dataset.perfPreset === active));
    b.disabled = !p[b.dataset.perfPreset];
  }
  // untouched: the engine's own values, which are no preset
  const byDefault = !active && samePerformance(PERF_DEFAULTS);
  setText($('perf-help'), active
    ? t(`app.perf.preset.${active}.help`)
    : byDefault
      ? t('app.perf.defaultHelp', { ram: gb(DEFAULT_RAM), vram: gb(DEFAULT_VRAM) })
      : t('app.perf.customHelp'));
  setText($('perf-note'), active ? t(`app.perf.preset.${active}.name`) : byDefault ? t('app.perf.default') : t('app.perf.custom'));

  // threads: 1 to twice the logical cores, or the engine's choice
  const auto = settings.threads === 'auto' || settings.threads === null || settings.threads === undefined;
  const threads = $('perf-threads');
  if (threads.max !== String(2 * n)) threads.max = String(2 * n);
  $('perf-threads-auto').checked = auto;
  threads.disabled = auto;
  const taken = server.job && server.job.settings ? server.job.settings.threads : null;
  setValue(threads, auto ? Math.min(taken || n, 2 * n) : Math.min(settings.threads, 2 * n));
  setText($('perf-threads-value'), auto ? t('app.perf.threadsAuto') : plural('app.perf.threadsOfCores', n, { threads: whole(settings.threads) }));

  // who draws, who shades
  const gpuTitle = card ? t('app.perf.card', { name: card.name }) : t('app.perf.cardUnnamed');
  const gpuDetail = card ? (card.memory_gb ? t('app.perf.cardMemory', { size: gb(card.memory_gb) }) : '') : m ? t('app.perf.noCard') : t('app.perf.lookingForCards');
  const noCard = (field) => Boolean(m) && !card && settings[field] !== 'gpu';
  renderChoice('renderer-choice', 'renderer', [
    ['auto', t('app.perf.autoTitle'), t('app.perf.renderer.autoDetail')],
    ['gpu', gpuTitle, gpuDetail, noCard('renderer')],
    ['cpu', t('app.perf.cpu'), t('app.perf.renderer.cpuDetail')],
  ]);
  renderChoice('shading-choice', 'shading', [
    ['auto', t('app.perf.autoTitle'), t('app.perf.shading.autoDetail')],
    ['gpu', gpuTitle, gpuDetail, noCard('shading')],
    ['cpu', t('app.perf.cpu'), ''],
  ]);

  // the memory budgets, bounded by the machine
  const ram = $('perf-ram');
  const maxRam = m && m.memory_gb ? Math.max(Math.floor(m.memory_gb), 1) : Math.max(64, settings.ram_gb || 0);
  if (ram.max !== String(maxRam)) ram.max = String(maxRam);
  const vRam = typeof settings.ram_gb === 'number' ? settings.ram_gb : null;
  setValue(ram, vRam === null ? DEFAULT_RAM : Math.min(vRam, maxRam));
  setText($('perf-ram-value'), vRam === null
    ? t('app.perf.sizeDefault', { size: gb(DEFAULT_RAM) })
    : m && m.memory_gb ? t('app.perf.sizeOf', { size: gb(vRam), total: gb(Math.round(m.memory_gb)) }) : gb(vRam));
  document.querySelector('[data-default="ram_gb"]').disabled = vRam === null;

  // the card's: no more than it can keep beside what the render puts on it (its threads, the shading)
  const vram = $('perf-vram');
  const room = textureRoom(settings, card);
  const maxVram = room !== null ? room : Math.max(24, settings.vram_gb || 0);
  if (vram.max !== String(maxVram)) vram.max = String(maxVram);
  const vVram = typeof settings.vram_gb === 'number' ? settings.vram_gb : null;
  setValue(vram, vVram === null ? DEFAULT_VRAM : Math.min(vVram, maxVram));
  vram.disabled = Boolean(m) && !card && vVram === null;
  setText($('perf-vram-value'), vVram === null
    ? (vram.disabled ? t('app.perf.noCardShort') : t('app.perf.sizeDefault', { size: gb(DEFAULT_VRAM) }))
    : card && card.memory_gb ? t('app.perf.sizeOf', { size: gb(vVram), total: gb(Math.round(card.memory_gb)) }) : gb(vVram));
  document.querySelector('[data-default="vram_gb"]').disabled = vVram === null;
  setText($('vram-warning'), cardWarning(vVram === null ? DEFAULT_VRAM : vVram, room, card));
  setHidden($('vram-warning'), !$('vram-warning').textContent);

  for (const b of $('priority-choice').children) b.setAttribute('aria-pressed', String(b.dataset.priority === (settings.priority || 'normal')));
  setText($('perf-next-note'), jobRunning() && isRender(server.job)
    ? t('app.perf.nextRunning')
    : t('app.perf.next'));
}

/**
 * Why the card may overflow with `budget` GB of textures (a render's settings), or '': more than
 * it can keep beside the render (a value set before, or by hand), or, while nothing runs, more
 * than what other programs leave of it now.
 */
function cardWarning(budget, room, card) {
  const on = onCard(settings, card);
  if (room === null || !on.drawing) return '';
  if (budget > room) {
    return t('app.perf.warning.overRoom', { size: gb(room) });
  }
  const l = server.load;
  if (jobRunning() || !l || typeof l.vram_used_gb !== 'number') return '';
  const taken = budget + drawingThreads(settings, card) * CARD_PER_THREAD + (on.shading ? CARD_SHADING : 0);
  if (l.vram_used_gb + taken <= card.memory_gb) return '';
  return t('app.perf.warning.otherPrograms', { used: gb(Math.round(l.vram_used_gb * 10) / 10), more: gb(Math.round(taken * 10) / 10), total: gb(Math.round(card.memory_gb)) });
}

// ------------------------------------------------------------------------------------- the load of the machine

/** The readings the chart shows, kept for its tooltip; the place of the pointer over it (0 to 1), or null. */
let historySeen = [];
let chartHover = null;
/** Seconds of the chart. */
const WINDOW_S = 120;

function gauge(id, valueId, v, say, total, missing) {
  const el = $(id);
  const read = typeof v === 'number' && Number.isFinite(v);
  el.classList.toggle('is-unknown', !read);
  setText($(valueId), read ? say(v) : '—');
  const pct = read && total ? Math.min(Math.max((v / total) * 100, 0), 100) : 0;
  el.querySelector('.gauge-track > i').style.width = `${pct.toFixed(1)}%`;
  el.title = read ? '' : missing;
}

function renderLoad() {
  const l = server.load;
  const m = machine();
  const card = mainCard();
  const waiting = t('app.load.firstReading');
  const ofTotal = (total) => (v) => (total ? t('app.load.usedOf', { used: decimal(v, 1), total: unit(total, 'gigabyte', 1) }) : unit(v, 'gigabyte', 1));
  gauge('gauge-cpu', 'gauge-cpu-value', l && l.cpu, percent, 100, l ? t('app.load.cpuUnreadable') : waiting);
  gauge('gauge-gpu', 'gauge-gpu-value', l && l.gpu, percent, 100, l ? t('app.load.gpuUnreadable') : waiting);
  gauge('gauge-vram', 'gauge-vram-value', l && l.vram_used_gb, ofTotal(card && card.memory_gb), card && card.memory_gb, l ? t('app.load.vramUnreadable') : waiting);
  gauge('gauge-ram', 'gauge-ram-value', l && l.ram_used_gb, ofTotal(m && m.memory_gb), m && m.memory_gb, l ? t('app.load.ramUnreadable') : waiting);
  $('gauge-gpu').querySelector('.gauge-name').title = card ? card.name : '';
  drawChart(l && Array.isArray(l.history) ? l.history : []);
}

/** The chart of the last two minutes: one path per series, broken where a reading is missing. */
function drawChart(history) {
  historySeen = history;
  const end = history.length ? history[history.length - 1][0] : 0;
  const path = (i) => {
    let d = '';
    let lifted = true;
    for (const point of history) {
      const x = WINDOW_S - (end - point[0]);
      const v = point[i];
      if (x < 0 || typeof v !== 'number') {
        lifted = true;
        continue;
      }
      d += `${lifted ? 'M' : 'L'}${x.toFixed(2)} ${(100 - Math.min(Math.max(v, 0), 100)).toFixed(2)}`;
      lifted = false;
    }
    return d;
  };
  $('chart-cpu').setAttribute('d', path(1));
  $('chart-gpu').setAttribute('d', path(2));
  if (chartHover !== null) chartTooltip(chartHover);
}

/** The tooltip of the chart: the reading nearest the pointer. */
function chartTooltip(f) {
  const tooltip = $('chart-tooltip');
  const h0 = historySeen;
  const hideAll = () => {
    setHidden(tooltip, true);
    $('chart-marker').setAttribute('d', '');
  };
  if (!h0.length) return hideAll();
  const end = h0[h0.length - 1][0];
  const wanted = end - WINDOW_S * (1 - f);
  let point = h0[0];
  for (const p of h0) if (Math.abs(p[0] - wanted) < Math.abs(point[0] - wanted)) point = p;
  if (Math.abs(point[0] - wanted) > 3) return hideAll();
  const x = WINDOW_S - (end - point[0]);
  $('chart-marker').setAttribute('d', `M${x.toFixed(2)} 0V100`);
  const age = Math.round(end - point[0]);
  const v = (n) => (typeof n === 'number' ? percent(n) : '—');
  tooltip.replaceChildren(
    h('b', null, age < 1 ? t('app.load.now') : t('app.load.ago', { duration: duration(age) })),
    h('i', { class: 'key key-cpu' }), t('app.load.cpu', { value: v(point[1]) }),
    h('i', { class: 'key key-gpu' }), t('app.load.gpu', { value: v(point[2]) }),
  );
  setHidden(tooltip, false);
  const width = tooltip.parentElement.clientWidth;
  const left = (x / WINDOW_S) * width - tooltip.offsetWidth / 2;
  tooltip.style.left = `${Math.max(Math.min(left, width - tooltip.offsetWidth), 0)}px`;
}

/** What the render really took (its `settings`): shown during the render and after it. */
function renderUsed(job) {
  const el = $('used');
  const show = isRender(job) && Boolean(job.settings || job.status === 'running');
  setHidden(el, !show);
  if (!show) return;
  const r = job.settings;
  // (what the card left to the processor changes the lines too, as it comes)
  const signature = JSON.stringify([r || null, job.fallbacks || [], job.asked || null]);
  if (el.dataset.signature === signature) return;
  el.dataset.signature = signature;
  if (!r) {
    $('used-list').replaceChildren(h('dd', { class: 'is-waiting' }, t('app.used.waiting')));
    return;
  }
  const cardUsed = r.renderer === 'gpu' || r.shading === 'gpu';
  // who really drew and shaded: the card as the engine names it, with its backend ("NVIDIA
  // GeForce RTX 4070 (Dx12)"), and the blocks it left to the processor
  const d = devices(job);
  const lines = [
    [t('app.used.threads'), whole(r.threads)],
    [t('app.used.drawing'), d.drawing],
    [t('app.used.shading'), d.shading],
    ...(d.chain ? [[t('app.used.chain'), d.chain]] : []),
    [t('app.used.ram'), gb(r.ram_gb)],
    [t('app.used.vram'), cardUsed ? gb(r.vram_gb) : t('app.used.vramUnused', { size: gb(r.vram_gb) })],
  ];
  $('used-list').replaceChildren(...lines.flatMap(([name, v]) => [h('dt', null, name), h('dd', null, v)]));
}

/**
 * The amber banner of the work the card did not do (during and after a render), and the line
 * beside the gauges that says who draws and who shades.
 */
function renderFallback(job) {
  const rendering = isRender(job);
  const alerts = rendering ? fallbackAlerts(job, machine()) : [];
  const banner = $('fallback-banner');
  const signature = JSON.stringify(alerts);
  if (banner.dataset.signature !== signature) {
    banner.dataset.signature = signature;
    banner.replaceChildren(...alerts.map(({ head, text }) => h('p', { class: 'fallback-banner-line' }, h('b', null, head), ' ', text)));
  }
  setHidden(banner, alerts.length === 0);

  const line = $('devices-line');
  let said = rendering ? devicesLine(job) : null;
  if (!said && rendering && job.status === 'running') said = t('app.used.devicesWaiting');
  setText(line, said || '');
  setHidden(line, !said);
  line.classList.toggle('is-fallback', alerts.length > 0);
  line.classList.toggle('is-gpu', Boolean(rendering && job.settings && job.settings.renderer === 'gpu' && !alerts.length));
}

// ------------------------------------------------------------------------------------- checks

/** The three checks of the state, in their order (their words are `app.check.<check>.name|title|missing`). */
const CHECKS = ['game', 'keys', 'config'];
let openPopover = null;

/** What a check found, in the language of the page (its `code` when the server gave one). */
const checkDetail = (c) => serverText(c.detail, c.code, c.params);

/** The gestures that repair a check (or change what it found). */
function checkActions(key, primary) {
  const cls = primary ? 'btn btn-sm btn-primary' : 'btn btn-sm';
  if (key === 'game') {
    return [
      h('button', { class: cls, type: 'button', onclick: chooseGame }, t('app.check.game.choose')),
      settings && settings.game !== null && h('button', { class: 'btn btn-sm', type: 'button', onclick: () => setGame(null) }, t('app.check.game.fivem')),
    ];
  }
  if (key === 'keys') return [h('button', { class: cls, type: 'button', disabled: jobRunning(), onclick: importKeys }, t('app.check.keys.import'))];
  return [
    h('button', { class: cls, type: 'button', onclick: () => openTarget('config') }, t('app.check.config.open')),
    h('button', { class: 'btn btn-sm', type: 'button', onclick: chooseConfig }, t('app.check.config.choose')),
  ];
}

function renderChecks() {
  const checks = server.checks || {};
  const host = $('checks');
  const signature = JSON.stringify([checks, settings && settings.game, jobRunning()]);
  if (host.dataset.signature === signature) return;
  host.dataset.signature = signature;

  host.replaceChildren(
    ...CHECKS.map((key) => {
      const c = checks[key] || { ok: false, detail: '' };
      return h('button', {
        class: `check ${c.ok ? 'is-ok' : 'is-failing'}`, type: 'button', 'data-check': key, 'aria-expanded': String(openPopover === key),
        title: checkDetail(c), onclick: (ev) => togglePopover(key, ev.currentTarget),
      }, h('i'), h('span', null, t(`app.check.${key}.name`)));
    }),
  );

  // one line per check that failed, with the gesture that repairs it
  $('alerts').replaceChildren(
    ...CHECKS
      .filter((key) => checks[key] && !checks[key].ok)
      .map((key) =>
        h('div', { class: 'alert', role: 'alert' },
          h('span', { class: 'alert-text' }, h('b', null, t(`app.check.${key}.missing`)), ' ', checkDetail(checks[key])),
          ...checkActions(key, true).filter(Boolean).slice(0, 1),
        ),
      ),
  );
  if (openPopover) fillPopover(openPopover);
}

function fillPopover(key) {
  const c = (server.checks || {})[key] || { ok: false, detail: '' };
  $('check-popover').replaceChildren(
    h('div', { class: 'row' }, h('h3', { class: 'grow' }, t(`app.check.${key}.title`)), h('span', { class: `badge ${c.ok ? 'badge-green' : 'badge-red'}` }, c.ok ? t('app.check.ok') : t('app.check.toFix'))),
    h('p', null, checkDetail(c) || (c.ok ? '' : t(`app.check.${key}.missing`))),
    // no path (a game folder not found): no line, not the text "null"
    ...(c.path ? [h('p', { class: 'path mono selectable' }, c.path)] : []),
    h('div', { class: 'row' }, ...checkActions(key, !c.ok).filter(Boolean)),
  );
}

function togglePopover(key, anchor) {
  const popover = $('check-popover');
  if (openPopover === key || !key) {
    openPopover = null;
    setHidden(popover, true);
  } else {
    openPopover = key;
    fillPopover(key);
    setHidden(popover, false);
    const r = anchor.getBoundingClientRect();
    popover.style.top = `${r.bottom + 8}px`;
    popover.style.left = `${Math.max(Math.min(r.left, window.innerWidth - popover.offsetWidth - 12), 12)}px`;
  }
  for (const b of $('checks').children) b.setAttribute('aria-expanded', String(b.dataset.check === openPopover));
}

/**
 * Opens the native file dialog of the program; null when the user gives up. `purpose`: what the
 * dialog is for (game, resources, config, magic or folder), which the demo answers by.
 */
async function choose(kind, title, startFolder, purpose) {
  if (choosing) return null;
  choosing = true;
  const waiting = toast(t('app.choose.open'), 'info', 0);
  try {
    const body = { kind, title, purpose };
    if (startFolder) body.start_folder = startFolder;
    const response = await api('POST', '/api/choose', body);
    return response.path || null;
  } catch (error) {
    report(error);
    return null;
  } finally {
    waiting.remove();
    choosing = false;
  }
}

function setGame(path) {
  togglePopover(null);
  settings.game = path;
  touch('game', 0);
}

async function chooseGame() {
  togglePopover(null);
  const c = (server.checks || {}).game;
  const path = await choose('folder', t('app.choose.game'), c && c.path ? c.path : undefined, 'game');
  if (path) setGame(path);
}

async function chooseConfig() {
  togglePopover(null);
  const path = await choose('file', t('app.choose.config'), settings.config, 'config');
  if (!path) return;
  settings.config = path;
  touch('config', 0);
}

async function importKeys() {
  togglePopover(null);
  const path = await choose('file', t('app.choose.magic'), undefined, 'magic');
  if (path) await launch({ kind: 'import-keys', file: path });
}

async function changeFolder(key) {
  const title = key === 'output' ? t('app.choose.output') : t('app.choose.preview');
  const path = await choose('folder', title, settings[key], 'folder');
  if (!path || path === settings[key]) return;
  settings[key] = path;
  touch(key, 0);
}

async function openTarget(target) {
  togglePopover(null);
  try {
    await api('POST', '/api/open', { target });
    if (demo) toast(t('app.demo.nothingOpened'));
  } catch (error) {
    report(error);
  }
}

// ------------------------------------------------------------------------------------- jobs

/**
 * Why a render (`action` 'render') or an update (`action` 'update') cannot start, or '' when it
 * can.
 */
function blocker(action = 'render') {
  if (!server) return t('app.blocker.waiting');
  if (!connected) return t('app.blocker.noAnswer');
  const c = server.checks || {};
  if (c.game && !c.game.ok) return t(`app.blocker.${action}.game`);
  // (the alert at the top says which key is missing and what to do: import magic.dat, or "Verify")
  if (c.keys && !c.keys.ok) return t(`app.blocker.${action}.keys`);
  if (c.config && !c.config.ok) return t(`app.blocker.${action}.config`);
  return '';
}

let launching = false;

async function launch(body) {
  if (launching) return;
  launching = true;
  try {
    // what was just changed in the settings is part of the job
    if (sending || touched.size) await sendSettings();
    await api('POST', '/api/launch', body);
    if (isList(body)) $('changes').open = true;
    else if (!isRender(body)) $('log').open = true;
  } catch (error) {
    report(error);
  } finally {
    launching = false;
  }
  poll();
}

/** Seconds a render of `blocks` blocks should take, or null when no speed is known. */
function estimate(blocks) {
  // about 15 s to read the game before the first block
  return knownSpeed ? blocks / knownSpeed + 15 : null;
}

function zoneBlocks(z) {
  return [Math.round((z[2] - z[0]) / BLOCK), Math.round((z[3] - z[1]) / BLOCK)];
}

async function launchMap() {
  const blocks = (server.sources.map && server.sources.map.blocks) || 6100;
  const estimated = estimate(blocks);
  const d = estimated
    ? plural('app.launchMap.estimate', blocks, { duration: roundDuration(estimated), speed: decimal(knownSpeed, 1) })
    : plural('app.launchMap.noEstimate', blocks);
  const agreed = await confirmDialog({
    title: t('app.launchMap.title'),
    lines: [
      t('app.launchMap.folder'),
      h('p', { class: 'path mono' }, settings.output || ''),
      t('app.launchMap.rewritten'),
      d,
    ],
    yes: t('app.launchMap.yes'),
  });
  if (agreed) await launch({ kind: 'map' });
}

async function stopJob() {
  const job = server && server.job;
  if (!job || job.status !== 'running') return;
  let lines = [t('app.stop.outputStays')];
  if (job.kind === 'update') lines = [t('app.stop.update')];
  else if (isRender(job)) lines = [t('app.stop.render')];
  const agreed = await confirmDialog({
    title: job.kind === 'update' ? t('app.stop.updateTitle') : isRender(job) ? t('app.stop.renderTitle') : t('app.stop.jobTitle'),
    lines,
    yes: t('app.stop.yes'),
    no: t('app.stop.no'),
    danger: true,
  });
  if (!agreed) return;
  try {
    await api('POST', '/api/stop');
  } catch (error) {
    report(error);
  }
  poll();
}

/** The folder of the whole map, as the settings name it. */
const mapFolder = () => (server && server.sources && server.sources.map && server.sources.map.folder) || (settings && settings.output) || '';

/** A file of the folder of the whole map, written with the folder's own separator. */
function mapFile(name) {
  const folder = mapFolder();
  const separator = folder.includes('\\') ? '\\' : '/';
  return folder ? `${folder.replace(/[\\/]+$/, '')}${separator}${name}` : name;
}

/**
 * Why no job of `update` can start (update, search of the changes, record), or '' when one can:
 * the checks of a render, then a map in the folder of the whole map.
 */
function updateBlocker() {
  const r = blocker('update');
  if (r) return r;
  const s = server.sources && server.sources.map;
  if (!s || !s.exists) return t('app.blocker.update.noMap', { folder: mapFolder() });
  return '';
}

/** "The map's tiles over <area> will be redrawn and replaced in <folder>." */
function replacementSentence(area) {
  return h('p', null, ...sentence('app.update.replace', { area }, { folder: h('span', { class: 'path mono' }, mapFolder()) }));
}

/** "Update the map here": the zone drawn, redrawn in the whole map itself. */
async function launchUpdateZone() {
  if (!zone || updateBlocker()) return;
  const z = [...zone];
  const [nx, ny] = zoneBlocks(z);
  const estimated = estimate(nx * ny);
  const agreed = await confirmDialog({
    title: t('app.update.zoneTitle'),
    lines: [
      replacementSentence(plural('app.update.thisZone', nx * ny)),
      [t('app.update.restUnchanged'), estimated ? t('app.estimate.about', { duration: roundDuration(estimated) }) : ''].filter(Boolean).join(' '),
    ],
    yes: t('app.update.yes'),
  });
  if (agreed) await launch({ kind: 'update', zone: z });
}

async function quit() {
  const agreed = await confirmDialog({
    title: t('app.quit.title'),
    lines: [jobRunning() ? t('app.quit.running') : t('app.quit.idle')],
    yes: t('app.quit.yes'),
    danger: jobRunning(),
  });
  if (!agreed) return;
  try {
    await api('POST', '/api/quit');
  } catch (error) {
    // no answer: it may have gone before answering; anything else is said
    if (error.status !== 0) {
      report(error);
      return;
    }
  }
  stopped = true;
  clearTimeout(pollTimer);
  document.title = t('app.documentTitle.idle');
  setHidden($('end-screen'), false);
  window.close();
}

// ------------------------------------------------------------------------------------- resource changes

/**
 * The last word of the server on what changed in the resources since the map recorded them (a
 * `job.changes`), kept while the page lives: { id, kind, status, message, changes } of the job
 * that said it. The server keeps only the last job: a later one does not erase it here.
 */
let changesSeen = null;
/** The last job seen after that list that may have made it stale: { id, kind }, or null. */
let changesAfter = null;
/** The row clicked, whose zone stays drawn on the map (its index), or null; the row under the pointer. */
let pinnedChange = null;
let hoveredChange = null;
let frameTimer = 0;

/** The jobs that make a list of changes stale (each has its sentence, `app.changes.stale.<kind>`). */
const STALE_KINDS = new Set(['update', 'map', 'record']);
const CHANGE_STATES = {
  added: ['app.changes.state.added', 'badge badge-green'],
  changed: ['app.changes.state.changed', 'badge badge-amber'],
  removed: ['app.changes.state.removed', 'badge badge-red'],
};

/** How each job seen while the page lives ended so far: id → { status, blocks } (blocks over). */
const outcomes = new Map();

/** What the job shown says of the changes, kept; and what a later job made stale. */
function noteChanges(job) {
  if (!job) return;
  outcomes.set(job.id, { status: job.status, blocks: job.blocks_done || 0 });
  const v = changesSeen;
  if (job.changes && (!v || job.id >= v.id)) {
    const other = !v || v.id !== job.id || JSON.stringify(v.changes) !== JSON.stringify(job.changes);
    changesSeen = { id: job.id, kind: job.kind, status: job.status, message: job.message, changes: job.changes };
    if (other) {
      changesAfter = null;
      pinnedChange = null;
      hoveredChange = null;
      map.setMarks(null);
    }
  } else if (v && job.id === v.id) {
    v.status = job.status;
    v.message = job.message;
  } else if (v && job.id > v.id && STALE_KINDS.has(job.kind)) {
    changesAfter = { id: job.id, kind: job.kind };
  }
}

/**
 * Whether job `id` may have changed the map or the record of its resources: not when it failed
 * or was stopped before any block (refused by the lock of another render, keys missing…). A job
 * this page never saw may have.
 */
function mayHaveChanged(id) {
  const o = outcomes.get(id);
  return !o || o.status === 'running' || o.status === 'done' || o.blocks > 0;
}

/**
 * What the section shows: { mode, … } (see `renderChanges`). `stale`: the kind of the job that
 * made the list stale, or ''.
 */
function changesModel() {
  const job = server.job;
  const running = jobRunning();
  const v = changesSeen;
  if (running && job.kind === 'list-changes') return { mode: 'searching' };
  if (running && job.kind === 'record') return { mode: 'recording' };
  // a search or a record that said nothing of the changes (refused by the program, failed early)
  if (job && !running && isList(job) && !job.changes && (!v || v.id < job.id)) {
    if (job.kind === 'record' && job.status === 'done') return { mode: 'recorded' };
    return { mode: 'failed', kind: job.kind, status: job.status, message: job.message };
  }
  if (!v) return { mode: 'nothing' };
  if (v.kind === 'record') return v.status === 'done' ? { mode: 'recorded' } : { mode: 'failed', kind: 'record', status: v.status, message: v.message };
  if (!v.changes.known) return { mode: 'no-record', changes: v.changes };
  const updating = running && job.kind === 'update' && job.id === v.id;
  // the list of an update (in progress or over) is what it redraws, not what is left to do; but
  // an update that found nothing to do, or failed before its first block, leaves it true
  const empty = !v.changes.zones.length && !v.changes.resources.length;
  const after = changesAfter && mayHaveChanged(changesAfter.id) ? changesAfter.kind : '';
  const stale = after || (v.kind === 'update' && !updating && !empty && mayHaveChanged(v.id) ? 'update' : '');
  return { mode: 'list', changes: v.changes, stale, updating };
}

/** Its ground: the blocks it touches and its corners. */
function zoneText(z) {
  const [nx, ny] = zoneBlocks(wholeBlocks(z));
  return plural('app.changes.zone', nx * ny, { x1: whole(z[0]), x2: whole(z[2]), y1: whole(z[1]), y2: whole(z[3]) });
}

/** "3 resources changed (1 added, 2 changed): 2 zones, 17 blocks to redraw." */
function changesSummary(changes) {
  const n = changes.resources.length;
  if (!n && !changes.zones.length) return t('app.changes.upToDate');
  const count = (state) => changes.resources.filter((r) => r.state === state).length;
  const parts = [
    [count('added'), 'app.changes.count.added'],
    [count('changed'), 'app.changes.count.changed'],
    [count('removed'), 'app.changes.count.removed'],
  ].filter(([k]) => k).map(([k, key]) => plural(key, k));
  const said = n
    ? (parts.length ? plural('app.changes.resourcesWithParts', n, { parts: parts.join(', ') }) : plural('app.changes.resources', n))
    : t('app.changes.noResource');
  if (!changes.zones.length) return t('app.changes.noZone', { summary: said });
  return t('app.changes.toRedraw', { summary: said, zones: plural('app.changes.zones', changes.zones.length), blocks: plural('app.changes.blocks', changes.blocks) });
}

/** The marks on the map: the row under the pointer, else the row clicked. */
function placeMarks() {
  const v = changesSeen;
  const i = hoveredChange !== null ? hoveredChange : pinnedChange;
  const r = v && i !== null ? v.changes.resources[i] : null;
  map.setMarks(r && r.zone ? [{ zone: r.zone, kind: r.state, name: r.name }] : null);
  for (const b of $('changes-list').querySelectorAll('.change')) b.classList.toggle('is-selected', Number(b.dataset.i) === pinnedChange);
}

function frameChange(i) {
  const r = changesSeen && changesSeen.changes.resources[i];
  if (r && r.zone) map.fitBounds(wholeBlocks(r.zone), { margins: mapMargins(60) });
}

function hoverChange(i) {
  hoveredChange = i;
  placeMarks();
  clearTimeout(frameTimer);
  // framed once the pointer rests on the row: passing over the list does not swing the map about
  if (i !== null) frameTimer = setTimeout(() => frameChange(i), 260);
}

function pickChange(i) {
  clearTimeout(frameTimer);
  pinnedChange = pinnedChange === i ? null : i;
  placeMarks();
  if (pinnedChange !== null) frameChange(i);
}

function changeRow(r, i) {
  const [stateKey, cls] = CHANGE_STATES[r.state] || [null, 'badge'];
  const word = stateKey ? t(stateKey) : String(r.state);
  const readable = Array.isArray(r.zone) && r.zone.length === 4;
  return h('li', null,
    h('button', {
      class: `change${readable ? '' : ' is-unreadable'}`, type: 'button', 'data-i': String(i), title: r.folder || r.name,
      'aria-disabled': readable ? null : 'true',
      onpointerenter: () => readable && hoverChange(i),
      onpointerleave: () => readable && hoverChange(null),
      onfocus: () => readable && hoverChange(i),
      onblur: () => readable && hoverChange(null),
      onclick: () => readable && pickChange(i),
    },
    h('span', { class: cls }, word),
    h('span', { class: 'change-name' }, r.name),
    h('span', { class: 'change-detail' }, readable ? zoneText(r.zone) : t('app.changes.unreadable', { reason: r.reason || t('app.changes.noReason') })),
    ),
  );
}

/** The section "Resource changes": the search, its list, the update and the record. */
function renderChanges() {
  const m = changesModel();
  const reason = updateBlocker();
  const busy = jobRunning();
  const find = $('find-changes');
  find.disabled = busy || Boolean(reason);
  find.title = reason || (busy ? t('app.changes.busy') : t('app.changes.findTitle'));

  // the sentence: what is known, with a spinner while it is being found
  const status = $('changes-status');
  let phrase = '';
  let cls = '';
  let waiting = false;
  if (m.mode === 'searching') [phrase, waiting] = [t('app.changes.searching'), true];
  else if (m.mode === 'recording') [phrase, waiting] = [t('app.changes.recording'), true];
  else if (m.mode === 'recorded') phrase = t('app.changes.recorded', { file: mapFile('resources.json') });
  else if (m.mode === 'failed') {
    const what = m.kind === 'record' ? 'record' : 'search';
    phrase = m.status === 'stopped'
      ? t(`app.changes.failed.${what}.stopped`)
      : t(`app.changes.failed.${what}.message`, { message: m.message || t('app.changes.failed.noMessage') });
    cls = 'is-error';
  } else if (m.mode === 'no-record') {
    phrase = t('app.changes.noRecord');
  } else if (m.mode === 'list') {
    phrase = changesSummary(m.changes);
    if (m.updating) [phrase, waiting] = [t('app.changes.updating', { summary: phrase }), true];
    else if (m.stale) [phrase, cls] = [t(`app.changes.stale.${m.stale}`), 'is-stale'];
  }
  if (reason && !busy && m.mode !== 'searching' && m.mode !== 'recording') {
    phrase = phrase ? `${phrase} ${reason}` : reason;
    if (!cls) cls = 'is-stale';
  }
  const signature = JSON.stringify([phrase, cls, waiting]);
  if (status.dataset.signature !== signature) {
    status.dataset.signature = signature;
    status.replaceChildren(...(phrase ? [waiting ? h('span', { class: 'spinner', 'aria-hidden': 'true' }) : null, h('span', null, phrase)].filter(Boolean) : []));
    status.className = `changes-status${cls ? ` ${cls}` : ''}`;
  }

  // the list, made again only when it changes (a row under the pointer keeps its place)
  const changes = m.changes || null;
  const list = $('changes-list');
  const listSignature = JSON.stringify(changes ? changes.resources : null);
  if (list.dataset.signature !== listSignature) {
    list.dataset.signature = listSignature;
    list.replaceChildren(...(changes ? changes.resources.map(changeRow) : []));
    placeMarks();
  }
  list.classList.toggle('is-stale', Boolean(m.stale));
  const note = $('changes-note');
  setText(note, (changes && changes.note) || '');
  setHidden(note, !(changes && changes.note));
  const count = $('changes-count');
  const n = m.mode === 'list' && !m.stale ? changes.resources.length : 0;
  setText(count, n ? whole(n) : '');
  setHidden(count, !n);

  // what can be done with it: the update of their ground, the record of the resources as they are
  const canUpdate = m.mode === 'list' && !m.stale && !m.updating && changes.zones.length > 0;
  const canRecord = m.mode === 'no-record' || (m.mode === 'list' && !m.stale && !m.updating && changes.resources.length > 0);
  const button = $('update-changes');
  setHidden(button, !canUpdate);
  if (canUpdate) setText(button, t('app.changes.updateButton', { zones: plural('app.changes.zones', changes.zones.length), blocks: plural('app.changes.blocks', changes.blocks) }));
  button.disabled = busy || Boolean(reason);
  button.title = reason || t('app.changes.updateTitle');
  const record = $('record-state');
  setHidden(record, !canRecord);
  record.className = m.mode === 'no-record' ? 'btn btn-sm btn-primary' : 'btn btn-sm';
  record.disabled = busy || Boolean(reason);
  record.title = reason || t('app.changes.recordTitle');
  setHidden($('changes-actions'), !canUpdate && !canRecord);
}

async function findChanges() {
  if (updateBlocker() || jobRunning()) return;
  await launch({ kind: 'list-changes' });
}

async function launchUpdateChanges() {
  const v = changesSeen;
  if (!v || updateBlocker()) return;
  const { zones, blocks } = v.changes;
  const estimated = estimate(blocks);
  const area = zones.length > 1
    ? plural('app.update.theseZones', zones.length, { blocks: plural('app.changes.blocks', blocks) })
    : plural('app.update.thisZone', blocks);
  const agreed = await confirmDialog({
    title: t('app.update.title'),
    lines: [
      replacementSentence(area),
      t('app.update.readAgain'),
      ...(estimated ? [t('app.estimate.about', { duration: roundDuration(estimated) })] : []),
    ],
    yes: t('app.update.yes'),
  });
  if (agreed) await launch({ kind: 'update', changes: true });
}

async function recordState() {
  if (updateBlocker()) return;
  const v = changesSeen;
  const changes = v && v.changes.known ? v.changes.resources.length : 0;
  const agreed = await confirmDialog({
    title: t('app.record.title'),
    lines: [
      h('p', null, ...sentence('app.record.where', {}, { file: h('span', { class: 'path mono' }, mapFile('resources.json')) })),
      changes ? plural('app.record.listed', changes) : t('app.record.when'),
    ],
    yes: t('app.record.yes'),
  });
  if (agreed) await launch({ kind: 'record' });
}

// ------------------------------------------------------------------------------------- progress: rendering

/** The title of each kind of job (`app.job.title.<key>`). */
const JOB_TITLE_KEYS = {
  zone: 'zone',
  map: 'map',
  verify: 'verify',
  groups: 'groups',
  resources: 'resources',
  'import-keys': 'importKeys',
  update: 'update',
  'list-changes': 'listChanges',
  record: 'record',
};
const STEPS = ['world', 'placements', 'blocks', 'zooms', 'done'];
/** The phases a job goes through (`app.phase.<phase>`). */
const PHASES = new Set(['starting', 'world', 'placements', 'blocks', 'zooms', 'done']);
const BADGES = {
  running: ['app.job.status.running', 'badge badge-accent badge-live'],
  done: ['app.job.status.done', 'badge badge-green'],
  stopped: ['app.job.status.stopped', 'badge badge-amber'],
  failed: ['app.job.status.failed', 'badge badge-red'],
};

/** Percentage of a render, or null while there is nothing to count. */
function percentage(job) {
  if (job.status === 'done') return 100;
  if (!job.blocks_total) return job.phase === 'zooms' ? 100 : null;
  if (job.status === 'running' && job.phase === 'zooms') return 100;
  return Math.min((job.blocks_done / job.blocks_total) * 100, 100);
}

function elapsed(job) {
  const ms = job.status === 'running' ? job.duration_ms + (performance.now() - receivedAt) : job.duration_ms;
  return duration((ms || 0) / 1000);
}

function renderActions() {
  const running = jobRunning();
  const reason = blocker();
  const reasonUpdate = server ? updateBlocker() : reason;
  setHidden($('launch-zone'), running);
  setHidden($('launch-map'), running);
  setHidden($('launch-update'), running);
  setHidden($('stop'), !running);
  $('launch-zone').disabled = Boolean(reason) || !zone;
  $('launch-map').disabled = Boolean(reason);
  $('launch-update').disabled = Boolean(reasonUpdate) || !zone;
  $('launch-zone').title = reason || (zone ? t('app.actions.zoneTitle') : t('app.actions.drawZoneFirst'));
  $('launch-map').title = reason || t('app.actions.mapTitle');
  $('launch-update').title = reasonUpdate || (zone ? t('app.actions.updateTitle') : t('app.actions.drawZoneFirst'));
  const note = $('actions-note');
  let message;
  if (running) {
    const job = server.job;
    if (job.kind === 'update') message = job.threads ? plural('app.actions.updateRunningOn', job.threads) : t('app.actions.updateRunning');
    else if (isRender(job)) message = job.threads ? plural('app.actions.renderRunningOn', job.threads) : t('app.actions.renderRunning');
    else if (isList(job)) message = t('app.actions.listRunning');
    else message = t('app.actions.toolRunning');
  } else if (reason) {
    message = reason;
  } else if (zone) {
    const [nx, ny] = zoneBlocks(zone);
    const estimated = estimate(nx * ny);
    const params = { blocks: plural('app.actions.blocks', nx * ny), width: whole(nx * BLOCK), height: whole(ny * BLOCK) };
    message = estimated ? t('app.actions.zoneEstimate', { ...params, duration: roundDuration(estimated) }) : t('app.actions.zone', params);
    // (the preview needs no map; an update does)
    if (reasonUpdate) message += ` ${reasonUpdate}`;
  } else {
    message = t('app.actions.drawZone');
  }
  setText(note, message);
  note.classList.toggle('is-blocked', Boolean(reason) && !running);
  for (const button of $('tools-list').querySelectorAll('[data-tool]')) {
    button.disabled = button.dataset.tool !== 'quit' && (running || !connected);
  }
}

function renderProgress() {
  const job = server.job;
  const progress = $('progress');
  progress.classList.toggle('no-job', !job);
  progress.classList.toggle('is-tool', Boolean(job) && !isRender(job));
  setHidden($('progress-empty'), Boolean(job));
  renderUsed(job);
  renderFallback(job);
  if (!job) {
    setText($('job-title'), t('app.job.none'));
    setHidden($('job-badge'), true);
    setHidden($('outcome'), true);
    setHidden($('errors'), true);
    document.title = t('app.documentTitle.idle');
    return;
  }
  setText($('job-title'), JOB_TITLE_KEYS[job.kind] ? t(`app.job.title.${JOB_TITLE_KEYS[job.kind]}`) : job.kind);
  const [badgeKey, cls] = BADGES[job.status] || [null, 'badge'];
  const badge = $('job-badge');
  setHidden(badge, false);
  setText(badge, badgeKey ? t(badgeKey) : job.status);
  if (badge.className !== cls) badge.className = cls;

  const rendering = isRender(job);
  const pct = rendering ? percentage(job) : null;
  if (rendering) {
    // the steps: done, running, or where it stopped
    const rank = job.status === 'done' ? STEPS.length : Math.max(STEPS.indexOf(job.phase), 0);
    for (const li of $('steps').children) {
      const i = STEPS.indexOf(li.dataset.step);
      li.classList.toggle('is-done', i < rank);
      li.classList.toggle('is-active', i === rank && job.status === 'running');
      li.classList.toggle('is-stopped', i === rank && job.status === 'stopped');
      li.classList.toggle('is-failed', i === rank && job.status === 'failed');
    }
    const bar = $('progress-bar');
    bar.classList.toggle('is-indeterminate', pct === null && job.status === 'running');
    bar.classList.toggle('is-finished', job.status === 'done');
    bar.classList.toggle('is-stopped', job.status === 'stopped');
    bar.classList.toggle('is-failed', job.status === 'failed');
    bar.firstElementChild.style.width = pct === null ? '' : `${pct.toFixed(1)}%`;
    bar.setAttribute('aria-valuenow', pct === null ? '0' : String(Math.floor(pct)));
    setText($('percent'), pct === null ? '—' : percent(Math.floor(pct)));
    setText($('blocks-text'), job.blocks_total
      ? plural(job.status === 'running' && job.phase === 'zooms' ? 'app.progress.blocksFarZooms' : 'app.progress.blocks', job.blocks_total, { done: whole(job.blocks_done) })
      : job.status === 'running' && PHASES.has(job.phase) ? t(`app.phase.${job.phase}`) : '');
    setText($('stat-speed'), typeof job.speed === 'number' ? t('app.stats.speed', { speed: decimal(job.speed, 1) }) : '—');
    setText($('stat-elapsed'), elapsed(job));
    setText($('stat-remaining'), job.status === 'running' && typeof job.remaining_s === 'number' ? t('app.stats.remaining', { duration: roundDuration(job.remaining_s) }) : '—');
    // (blocks drawn, waiting to be written or being written: more of them than drawing threads)
    setText($('stat-running'), job.status === 'running'
      ? (job.threads ? plural('app.stats.runningOfThreads', job.threads, { running: whole(job.blocks_running || 0) }) : whole(job.blocks_running || 0))
      : '—');
    const tiles = job.tiles || { jpg: 0, png: 0, empty: 0 };
    // (`jpg` and `png` of the events: the full tiles and the partly empty ones, whatever their format)
    setText($('stat-tiles'), t('app.stats.tiles', { full: whole(tiles.jpg), partial: whole(tiles.png), empty: plural('app.stats.emptyTiles', tiles.empty) }));
    const errors = $('stat-errors');
    setText(errors, job.errors ? whole(job.errors) : t('app.stats.noErrors'));
    errors.classList.toggle('is-error', job.errors > 0);
  }

  const errorCount = Math.max(job.errors || 0, blockErrors.length);
  setHidden($('errors'), !blockErrors.length);
  setText($('errors-title'), plural('app.errors.title', errorCount));

  // the end of the job
  const outcome = $('outcome');
  setHidden(outcome, job.status === 'running');
  if (job.status !== 'running') {
    outcome.classList.toggle('is-stopped', job.status === 'stopped');
    outcome.classList.toggle('is-failed', job.status === 'failed');
    let phrase;
    const time = duration((job.duration_ms || 0) / 1000);
    const errorsSaid = job.errors ? plural('app.outcome.errors', job.errors) : t('app.outcome.noError');
    if (job.status === 'failed') {
      phrase = t('app.outcome.failed', { time, message: job.message || t('app.outcome.failedNoMessage') });
    } else if (job.status === 'stopped' && job.kind === 'update') {
      phrase = job.blocks_total
        ? plural('app.outcome.stoppedUpdateOf', job.blocks_total, { time, done: whole(job.blocks_done) })
        : plural('app.outcome.stoppedUpdate', job.blocks_done, { time });
    } else if (job.status === 'stopped') {
      phrase = rendering
        ? (job.blocks_total
          ? plural('app.outcome.stoppedRenderOf', job.blocks_total, { time, done: whole(job.blocks_done) })
          : plural('app.outcome.stoppedRender', job.blocks_done, { time }))
        : t('app.outcome.stopped', { time });
    } else if (job.kind === 'update' && !job.blocks_total) {
      // (the ground of what changed may be no block at all)
      phrase = t('app.outcome.updateNothing', { time });
    } else if (job.kind === 'update') {
      const tiles = job.tiles || { jpg: 0, png: 0 };
      phrase = t('app.outcome.updated', { time, blocks: plural('app.outcome.blocksRedrawn', job.blocks_done), tiles: plural('app.outcome.tilesReplaced', tiles.jpg + tiles.png), errors: errorsSaid });
    } else if (job.kind === 'list-changes') {
      phrase = t('app.outcome.listChanges', { time });
    } else if (job.kind === 'record') {
      phrase = t('app.outcome.recorded', { time });
    } else if (rendering) {
      const tiles = job.tiles || { jpg: 0, png: 0 };
      phrase = t('app.outcome.rendered', { time, blocks: plural('app.outcome.blocks', job.blocks_done), tiles: plural('app.outcome.tiles', tiles.jpg + tiles.png), errors: errorsSaid });
    } else {
      phrase = t('app.outcome.tool', { time });
    }
    setText($('outcome-text'), phrase);
    setHidden($('outcome-open'), !rendering || !job.source);
  }

  document.title = rendering && job.status === 'running' ? t('app.documentTitle.running', { percent: percent(pct === null ? 0 : Math.floor(pct)) }) : t('app.documentTitle.idle');
}

/** One line of the list of failed blocks. */
const blockErrorLine = (ev) => h('li', null,
  h('button', { type: 'button', title: t('app.errors.showBlock'), onclick: () => showBlock(ev.x, ev.y) }, t('app.errors.block', { x: String(ev.x), y: String(ev.y) })),
  ` — ${ev.message || t('app.errors.noMessage')}`,
);

function addBlockError(ev) {
  blockErrors.push(ev);
  const list = $('errors-list');
  if (blockErrors.length > 200) {
    if (blockErrors.length === 201) list.append(h('li', null, t('app.errors.more')));
    return;
  }
  list.append(blockErrorLine(ev));
}

/** The list of failed blocks made again (in another language). */
function renderBlockErrors() {
  const lines = blockErrors.slice(0, 200).map(blockErrorLine);
  if (blockErrors.length > 200) lines.push(h('li', null, t('app.errors.more')));
  $('errors-list').replaceChildren(...lines);
}

function showBlock(bx, by) {
  map.fitBounds([(bx - 3) * BLOCK, (by - 3) * BLOCK, (bx + 4) * BLOCK, (by + 4) * BLOCK], { margins: mapMargins(40) });
}

function expandLog(expanded) {
  const el = $('log');
  if (expanded) el.open = true;
  el.classList.toggle('is-expanded', expanded);
  renderLogExpand();
}

function renderLogExpand() {
  const expanded = $('log').classList.contains('is-expanded');
  $('log-expand').setAttribute('aria-pressed', String(expanded));
  setText($('log-expand'), expanded ? t('app.log.reduce') : t('app.log.enlarge'));
}

/** Adds lines to the log; it follows its end unless the user scrolled up. */
function appendLog(lines) {
  const el = $('log-lines');
  log.lines.push(...lines);
  const atEnd = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
  if (log.lines.length > 4000) {
    // bounded: the oldest lines go
    log.lines = log.lines.slice(-3000);
    log.cut = true;
    renderLog();
  } else {
    el.append(document.createTextNode(`${lines.join('\n')}\n`));
  }
  log.shown = log.lines.length;
  setText($('log-count'), whole(log.shown));
  if (atEnd) el.scrollTop = el.scrollHeight;
}

/** The whole log written again, after its start was cut (the mark of the cut is page text). */
function renderLog() {
  $('log-lines').replaceChildren(document.createTextNode(`${t('app.log.cut')}\n${log.lines.join('\n')}\n`));
  setText($('log-count'), whole(log.shown));
}

// ------------------------------------------------------------------------------------- map: around the canvas

/**
 * Room the floating panels take over the map, so that a framed area stays clear of them.
 * `legend`: the legend of a render is (or is about to be) under the toolbar.
 */
function mapMargins(margin, legend = !$('legend').hidden) {
  return [margin + (legend ? 92 : 52), margin, margin + 44, margin];
}

function showSource(name) {
  shownSource = name;
  map.setSource(name);
  for (const b of $('source-toggle').children) b.setAttribute('aria-pressed', String(b.dataset.source === name));
  if (server) renderSource();
}

function renderSource() {
  const sources = server.sources || {};
  const s = sources[shownSource] || {};
  const job = server.job;
  const rendering = jobRunning() && job.source === shownSource;
  // under a preview, the whole map stays faintly visible: one knows where the zone is
  map.setBackground(shownSource === 'preview' && sources.map && sources.map.exists ? 'map' : null);
  let status;
  if (rendering) status = job.kind === 'update' ? t('app.source.updating') : t('app.source.rendering');
  else if (!s.exists) status = t('app.source.noTiles');
  else if (s.complete) status = s.blocks ? plural('app.source.blocks', s.blocks) : t('app.source.complete');
  else status = t('app.source.incomplete');
  setText($('source-status'), status);
  $('source-status').title = s.folder || '';
  const empty = $('map-empty');
  setHidden(empty, Boolean(s.exists) || rendering);
  setText(empty, shownSource === 'preview' ? t('app.source.noPreview') : t('app.source.noMap'));
  setHidden($('legend'), !planKnown);
}

/** Where "Zoom to fit" goes: the tiles of the shown source, else the plan, else San Andreas. */
function visibleBounds() {
  const s = server && server.sources ? server.sources[shownSource] : null;
  if (s && s.exists && Array.isArray(s.bounds) && s.bounds.length === 4) return s.bounds;
  return map.planBounds() || SAN_ANDREAS;
}

function firstView() {
  // a render that is running frames itself (its zone, or its plan)
  if (jobRunning() && isRender(server.job)) return;
  const view = readStorage(STORAGE.view);
  if (view && [view.cx, view.cy, view.k].every(Number.isFinite)) map.setView(view);
  else map.fitBounds(visibleBounds(), { animate: false, margins: mapMargins(24) });
}

let viewTimer = 0;
let hudFrame = 0;

function onView() {
  // remembered only once the first answer came: until then the view is the placeholder framing,
  // and a slow first answer would have it written over the view remembered from last time
  if (server) {
    clearTimeout(viewTimer);
    viewTimer = setTimeout(() => {
      const v = map.view();
      writeStorage(STORAGE.view, { cx: v.cx, cy: v.cy, k: v.k });
    }, 500);
  }
  if (hudFrame) return;
  hudFrame = requestAnimationFrame(() => {
    hudFrame = 0;
    renderScale();
  });
}

/** The zoom and the scale bar of the HUD. */
function renderScale() {
  const v = map.view();
  setText($('hud-zoom'), decimal(v.zoom, 1));
  const { metres, px } = map.scale(120);
  $('scale-bar').style.width = `${Math.round(px)}px`;
  setText($('scale-text'), metres >= 1000 ? unit(metres / 1000, 'kilometer', 0) : unit(metres, 'meter', 0));
}

/** The state of a block under the pointer, as the map names it (`app.blockState.<state>`). */
const BLOCK_STATES = new Set(['waiting', 'running', 'done', 'failed']);
/** The last place under the pointer, written again in another language. */
let lastHover = null;

function onHover(p) {
  lastHover = p;
  setText($('hud-x'), p ? whole(p.x) : '—');
  setText($('hud-y'), p ? whole(p.y) : '—');
  setText($('hud-block'), p
    ? (p.state ? t('app.hud.blockState', { bx: String(p.bx), by: String(p.by), state: BLOCK_STATES.has(p.state) ? t(`app.blockState.${p.state}`) : String(p.state) }) : t('app.hud.block', { bx: String(p.bx), by: String(p.by) }))
    : '—');
}

function onZone(z) {
  zone = z;
  setHidden($('clear-zone'), !z);
  setHidden($('frame-zone'), !z);
  renderActions();
}

function onMode(active) {
  $('pick-zone').setAttribute('aria-pressed', String(active));
  setHidden($('zone-help'), !active);
}

// ------------------------------------------------------------------------------------- everything

function render() {
  if (!server) return;
  useLanguage(settingsLanguage());
  renderLanguageSelect();
  setText($('version'), server.version ? `v${server.version}` : '');
  renderChecks();
  renderSettings();
  renderSource();
  renderActions();
  renderProgress();
  renderChanges();
  renderLoad();
}

// ------------------------------------------------------------------------------------- wiring

function wireSettings() {
  const sliders = [
    ['lighting-azimuth', 'azimuth'],
    ['lighting-elevation', 'elevation'],
    ['lighting-strength', 'strength'],
    ['lighting-ambient-occlusion', 'ambient_occlusion'],
  ];
  for (const [id, field] of sliders) {
    $(id).addEventListener('input', (ev) => settings && setField('lighting', field, Math.round(Number(ev.target.value) * 100) / 100));
  }
  $('lighting-enabled').addEventListener('change', (ev) => settings && setField('lighting', 'enabled', ev.target.checked, 0));
  $('lighting-shadows').addEventListener('change', (ev) => settings && setField('lighting', 'shadows', ev.target.checked, 0));
  for (const button of document.querySelectorAll('[data-lighting-preset]')) {
    button.addEventListener('click', () => {
      if (!settings) return;
      const [elevation, strength] = button.dataset.lightingPreset === 'low' ? [35, 0.6] : [45, 0.5];
      setField('lighting', 'elevation', elevation, 0);
      setField('lighting', 'strength', strength, 0);
    });
  }

  // lists typed one name per line: sent a moment after the last key, and when the field is left
  const lists = [
    ['maps-keep', 'maps', 'keep'],
    ['maps-exclude-folders', 'maps', 'exclude_folders'],
  ];
  for (const [id, section, field] of lists) {
    $(id).addEventListener('input', (ev) => settings && setField(section, field, linesOf(ev.target.value), 900));
    $(id).addEventListener('blur', () => {
      if (!settings) return;
      renderSettings();
      if (touched.size) scheduleSend(0);
    });
  }

  for (const button of document.querySelectorAll('[data-revert]')) {
    button.addEventListener('click', () => {
      if (!settings) return;
      const what = button.dataset.revert;
      const o = settings.overrides;
      if (what === 'lighting') delete o.lighting;
      else if (what === 'water') delete o.water;
      else if (what === 'fill-holes' && o.maps) delete o.maps.fill_holes;
      else if (what === 'keep' && o.maps) delete o.maps.keep;
      else if (what === 'exclude-folders' && o.maps) delete o.maps.exclude_folders;
      else if (what === 'resources') delete o.resources;
      // the text areas take map.toml's lists back even when one of them has the focus
      if (document.activeElement instanceof HTMLTextAreaElement) document.activeElement.blur();
      touch('overrides', 0);
    });
  }

  $('resources-add-folder').addEventListener('click', addResource);
  $('water-enabled').addEventListener('change', (ev) => settings && setField('water', 'enabled', ev.target.checked, 0));
  $('maps-fill-holes').addEventListener('change', (ev) => settings && setField('maps', 'fill_holes', ev.target.checked, 0));

  // performance
  for (const button of document.querySelectorAll('[data-perf-preset]')) button.addEventListener('click', () => applyPreset(button.dataset.perfPreset));
  $('perf-threads').addEventListener('input', (ev) => setSetting('threads', Number(ev.target.value), 350));
  $('perf-threads-auto').addEventListener('change', (ev) => setSetting('threads', ev.target.checked ? 'auto' : Number($('perf-threads').value)));
  $('perf-ram').addEventListener('input', (ev) => setSetting('ram_gb', Number(ev.target.value), 350));
  $('perf-vram').addEventListener('input', (ev) => setSetting('vram_gb', Number(ev.target.value), 350));
  for (const button of document.querySelectorAll('[data-default]')) button.addEventListener('click', () => setSetting(button.dataset.default, null));
  for (const button of $('priority-choice').children) button.addEventListener('click', () => setSetting('priority', button.dataset.priority));

  // the format of the tiles
  $('quality').addEventListener('input', (ev) => setSetting('quality', Number(ev.target.value), 350));
  $('lossless').addEventListener('change', (ev) => setSetting('quality', ev.target.checked ? 'lossless' : DEFAULT_QUALITY));

  // the tooltip of the chart of the load
  const plot = document.querySelector('.chart-plot');
  plot.addEventListener('pointermove', (ev) => {
    const r = plot.getBoundingClientRect();
    chartHover = Math.min(Math.max((ev.clientX - r.left) / Math.max(r.width, 1), 0), 1);
    chartTooltip(chartHover);
  });
  plot.addEventListener('pointerleave', () => {
    chartHover = null;
    setHidden($('chart-tooltip'), true);
    $('chart-marker').setAttribute('d', '');
  });
  for (const button of document.querySelectorAll('[data-change]')) button.addEventListener('click', () => settings && changeFolder(button.dataset.change));
  for (const button of document.querySelectorAll('[data-open]')) button.addEventListener('click', () => openTarget(button.dataset.open));

  // the language of the page, a setting of the program like the others
  $('language-select').addEventListener('change', (ev) => {
    const code = ev.target.value;
    useLanguage(code);
    if (settings) {
      settings.language = code;
      touch('language', 0);
    }
  });

  // what is folded is remembered
  const folds = readStorage(STORAGE.sections) || {};
  const foldable = [...document.querySelectorAll('details[data-section]'), $('maps-keep-details'), $('log'), $('changes')];
  for (const el of foldable) {
    const key = el.dataset.section || el.id;
    if (typeof folds[key] === 'boolean' && key !== 'log') el.open = folds[key];
    el.addEventListener('toggle', () => {
      const stored = readStorage(STORAGE.sections) || {};
      stored[key] = el.open;
      writeStorage(STORAGE.sections, stored);
      if (el === $('log') && el.open) $('log-lines').scrollTop = $('log-lines').scrollHeight;
      if (el === $('log') && !el.open) expandLog(false);
    });
  }
}

function wireSidebar() {
  const app = $('app');
  const button = $('settings-toggle');
  const narrow = window.matchMedia('(max-width: 1180px)');
  const setSidebar = (open) => {
    app.classList.toggle('no-settings', !open);
    button.setAttribute('aria-expanded', String(open));
  };
  // a narrow window starts with the column folded; a wide one as it was left
  const initial = () => setSidebar(narrow.matches ? false : readStorage(STORAGE.sidebar) !== false);
  initial();
  narrow.addEventListener('change', initial);
  button.addEventListener('click', () => {
    const open = app.classList.contains('no-settings');
    setSidebar(open);
    if (!narrow.matches) writeStorage(STORAGE.sidebar, open);
  });
  $('settings-scrim').addEventListener('click', () => setSidebar(false));
}

function wireMap() {
  $('zoom-in').addEventListener('click', () => map.zoomBy(2));
  $('zoom-out').addEventListener('click', () => map.zoomBy(0.5));
  $('zoom-all').addEventListener('click', () => map.fitBounds(visibleBounds(), { margins: mapMargins(24) }));
  for (const b of $('source-toggle').children) b.addEventListener('click', () => showSource(b.dataset.source));
  $('pick-zone').addEventListener('click', () => {
    const active = $('pick-zone').getAttribute('aria-pressed') !== 'true';
    map.setPickMode(active);
    onMode(active);
  });
  $('clear-zone').addEventListener('click', () => map.setZone(null));
  $('frame-zone').addEventListener('click', () => zone && map.fitBounds(zone, { margins: mapMargins(60) }));
  $('mosaic-toggle').addEventListener('click', () => {
    const visible = $('mosaic-toggle').getAttribute('aria-pressed') !== 'true';
    $('mosaic-toggle').setAttribute('aria-pressed', String(visible));
    map.showMosaic(visible);
  });
}

function wireActions() {
  $('launch-zone').addEventListener('click', () => zone && launch({ kind: 'zone', zone }));
  $('launch-map').addEventListener('click', launchMap);
  $('launch-update').addEventListener('click', launchUpdateZone);
  $('stop').addEventListener('click', stopJob);

  // the resource changes: the search, the update of their ground (whose zones show on the map
  // while the pointer is over it), the record of the resources as they are
  $('find-changes').addEventListener('click', findChanges);
  $('update-changes').addEventListener('click', launchUpdateChanges);
  $('record-state').addEventListener('click', recordState);
  const showZones = (yes) => {
    const v = changesSeen;
    if (yes && v) map.setMarks(v.changes.zones.map((z) => ({ zone: z, kind: 'zone' })));
    else placeMarks();
  };
  $('update-changes').addEventListener('pointerenter', () => showZones(true));
  $('update-changes').addEventListener('pointerleave', () => showZones(false));
  $('update-changes').addEventListener('focus', () => showZones(true));
  $('update-changes').addEventListener('blur', () => showZones(false));
  $('outcome-open').addEventListener('click', () => {
    const job = server && server.job;
    if (job && job.source) openTarget(job.source === 'map' ? 'output' : 'preview');
  });

  // the log, enlarged over the window or back in its column
  $('log-expand').addEventListener('click', (ev) => {
    ev.preventDefault();
    expandLog(!$('log').classList.contains('is-expanded'));
  });

  // tools menu
  const button = $('tools-button');
  const list = $('tools-list');
  const close = () => {
    setHidden(list, true);
    button.setAttribute('aria-expanded', 'false');
  };
  button.addEventListener('click', () => {
    const opened = list.hidden;
    setHidden(list, !opened);
    button.setAttribute('aria-expanded', String(opened));
    if (opened) list.querySelector('button:not(:disabled)').focus();
  });
  list.addEventListener('click', (ev) => {
    const tool = ev.target.closest('[data-tool]');
    if (!tool) return;
    close();
    if (tool.dataset.tool === 'quit') quit();
    else launch({ kind: tool.dataset.tool });
  });

  // the confirmation: its two buttons, and Escape (which closes the dialog by itself) is a no
  $('dialog-yes').addEventListener('click', () => answerDialog && answerDialog(true));
  $('dialog-no').addEventListener('click', () => answerDialog && answerDialog(false));
  $('dialog').addEventListener('cancel', () => answerDialog && answerDialog(false));
  $('dialog').addEventListener('close', () => answerDialog && answerDialog(false));

  // a click elsewhere, or Escape, closes what floats
  document.addEventListener('pointerdown', (ev) => {
    if (!ev.target.closest('#tools-menu')) close();
    if (openPopover && !ev.target.closest('#check-popover') && !ev.target.closest('.check')) togglePopover(null);
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape' || $('dialog').open) return;
    close();
    expandLog(false);
    if (openPopover) togglePopover(null);
    if ($('pick-zone').getAttribute('aria-pressed') === 'true') {
      map.setPickMode(false);
      onMode(false);
    }
  });
}

// ------------------------------------------------------------------------------------- start

async function start() {
  let tileUrls = (source, z, tx, ty, rev) => [`tiles/${source}/${z}/${tx}/${ty}?v=${rev}`];
  if (query.has('demo')) {
    const { createDemo } = await import('./demo.js');
    demo = createDemo(query);
    transport = demo.request;
    tileUrls = demo.tileUrls;
    setHidden($('demo-badge'), false);
  }
  // no tile is asked before the program has said what each folder of tiles holds
  const extents = { map: NO_TILE, preview: NO_TILE };
  map = createMap($('map'), { tileUrls, extents, onView, onHover, onZone, onMode });
  // in demonstration mode the map can be driven from the console: btmsDemo.map
  if (demo) window.btmsDemo.map = map;
  map.fitBounds(SAN_ANDREAS, { animate: false, margins: mapMargins(24) });

  addButtonIcons();
  wireSidebar();
  wireSettings();
  wireMap();
  wireActions();
  onView();
  // the browser's language until the settings say which one (they are read with the first state)
  useLanguage(null);

  // the elapsed time moves between two answers of the server
  setInterval(() => {
    if (jobRunning() && isRender(server.job)) setText($('stat-elapsed'), elapsed(server.job));
  }, 1000);

  window.addEventListener('error', (ev) => toast(t('app.pageError', { message: ev.message }), 'error', 12000));
  window.addEventListener('unhandledrejection', (ev) => toast(t('app.pageError', { message: ev.reason && ev.reason.message ? ev.reason.message : ev.reason }), 'error', 12000));

  poll();
}

start();
