//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// The languages of the interface. The dictionaries are `locales/<code>.json` next to the `js/`
// folder: flat objects with dotted keys namespaced by the file that owns them (`page.*` for the
// static text of index.html, `app.*`, `map.*`, `fallback.*`, `common.*`, `server.*`, `demo.*`).
// `en.json` is the source of truth. A text may hold `{name}` placeholders; a text that depends on
// a count has one key per plural category of the language (`key.one`, `key.other`, and `key.few`
// or `key.many` where the language needs them, as Russian does). A key missing from the chosen
// language falls back to English, and then to the key itself.
//
// The chosen language is a setting of the server, not of the browser: the port, and so the
// origin of the page, can change from one run to the next. The page gives it to `setLanguage` on
// every state; `null` (or no setting) means the first language of the browser that BTMS knows,
// else English. Until the first state, the browser's language is used.
//
// Static text of the page carries `data-i18n` (the text of the element), `data-i18n-title`,
// `data-i18n-aria-label` or `data-i18n-placeholder`, each naming a key, and optionally
// `data-i18n-params` (a JSON object of placeholder values). `applyTranslations` fills them; when
// a key is in no dictionary, the element keeps the English text written in the page, which is
// also what a browser without scripts shows.

/** The languages of the interface: code → native name, in the order of the language list. */
export const LANGUAGES = Object.freeze({
  en: 'English',
  fr: 'Français',
  de: 'Deutsch',
  nl: 'Nederlands',
  ru: 'Русский',
  sv: 'Svenska',
  es: 'Español',
});

/** The language every other one falls back to, and the one the dictionaries are written from. */
export const FALLBACK = 'en';

/** The event sent on `document` after the language changed; its `detail` is `{ language }`. */
export const LANGUAGE_EVENT = 'btms-language';

const ATTRIBUTES = [
  ['data-i18n-title', 'title'],
  ['data-i18n-aria-label', 'aria-label'],
  ['data-i18n-placeholder', 'placeholder'],
];
const SELECTOR = '[data-i18n], [data-i18n-title], [data-i18n-aria-label], [data-i18n-placeholder]';

/** The dictionaries asked for, code → promise of the flat object (never rejected). */
const loading = new Map();
/** The dictionaries loaded, code → flat object. */
const dictionaries = new Map();
const pluralRules = new Map();
const numberFormats = new Map();
const listeners = new Set();

let current = FALLBACK;
/** Whether the language in use came from the browser (no setting). */
let automatic = true;
/** The number of the last `setLanguage` call: an older call that finishes later changes nothing. */
let request = 0;

/** The code of the language in use (`en`, `fr`, …). */
export function language() {
  return current;
}

/**
 * The language of the browser that BTMS knows: the first of `navigator.languages` whose first
 * two letters name one of `LANGUAGES`, else English.
 */
export function browserLanguage() {
  const wanted = typeof navigator === 'undefined' ? [] : navigator.languages || [navigator.language];
  for (const tag of wanted) {
    const code = String(tag || '').slice(0, 2).toLowerCase();
    if (Object.hasOwn(LANGUAGES, code)) return code;
  }
  return FALLBACK;
}

/** The code BTMS uses for a setting: a known code as it is, `null`, `''` or `auto` for the browser's. */
function resolve(code) {
  if (code === null || code === undefined || code === '' || code === 'auto') return browserLanguage();
  const text = String(code).toLowerCase();
  if (Object.hasOwn(LANGUAGES, text)) return text;
  const short = text.slice(0, 2);
  return Object.hasOwn(LANGUAGES, short) ? short : FALLBACK;
}

/**
 * Loads a dictionary once. A file that cannot be read gives an empty one, kept as it is: asking
 * again on every state would not make it readable.
 */
function load(code) {
  if (dictionaries.has(code)) return Promise.resolve(dictionaries.get(code));
  if (loading.has(code)) return loading.get(code);
  const url = new URL(`../locales/${code}.json`, import.meta.url);
  const promise = fetch(url, { cache: 'no-store' })
    .then((reply) => {
      if (!reply.ok) throw new Error(`HTTP ${reply.status}`);
      return reply.json();
    })
    .then((dictionary) => {
      const flat = dictionary && typeof dictionary === 'object' && !Array.isArray(dictionary) ? dictionary : {};
      dictionaries.set(code, flat);
      return flat;
    })
    .catch((error) => {
      console.warn(`BTMS: the ${code} dictionary could not be read (${error.message}); English is shown instead.`);
      dictionaries.set(code, {});
      return {};
    })
    .finally(() => loading.delete(code));
  loading.set(code, promise);
  return promise;
}

/**
 * Uses a language: `code` is one of `LANGUAGES`, or `null` / `auto` for the browser's. Loads its
 * dictionary (and the English one), sets `<html lang>`, fills the static text of the page and
 * tells the listeners. Calling it again with the language in use costs nothing, so the page can
 * call it on every state. Resolves to the code in use.
 */
export async function setLanguage(code) {
  const wanted = resolve(code);
  automatic = code === null || code === undefined || code === '' || code === 'auto';
  const mine = ++request;
  if (wanted === current && dictionaries.has(wanted) && dictionaries.has(FALLBACK)) return current;
  await Promise.all([load(FALLBACK), load(wanted)]);
  // a later call won: its language is the one to show
  if (mine !== request) return current;
  const changed = wanted !== current;
  current = wanted;
  if (typeof document !== 'undefined') {
    document.documentElement.lang = current;
    applyTranslations(document);
  }
  if (changed || mine === 1) tell();
  return current;
}

/** Calls `listener(code)` after every change of language; returns the function that stops it. */
export function onLanguageChange(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function tell() {
  for (const listener of listeners) {
    try {
      listener(current);
    } catch (error) {
      console.error(error);
    }
  }
  if (typeof document !== 'undefined') {
    document.dispatchEvent(new CustomEvent(LANGUAGE_EVENT, { detail: { language: current } }));
  }
}

function plural(code, count) {
  let rules = pluralRules.get(code);
  if (!rules) {
    rules = new Intl.PluralRules(code);
    pluralRules.set(code, rules);
  }
  return rules.select(count);
}

/** The text of a key in one dictionary: the plural form when a count is given, else the key itself. */
function find(code, key, count) {
  const dictionary = dictionaries.get(code);
  if (!dictionary) return undefined;
  if (typeof count === 'number' && Number.isFinite(count)) {
    const form = dictionary[`${key}.${plural(code, count)}`] ?? dictionary[`${key}.other`];
    if (typeof form === 'string') return form;
  }
  const text = dictionary[key];
  return typeof text === 'string' ? text : undefined;
}

/** The template of a key: the chosen language, then English; `undefined` when neither has it. */
function template(key, params) {
  const count = params ? params.count : undefined;
  return find(current, key, count) ?? (current === FALLBACK ? undefined : find(FALLBACK, key, count));
}

/** Puts the values in the `{name}` places; numbers are written as the language writes them. */
function fill(text, params) {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name) => {
    if (!Object.hasOwn(params, name)) return whole;
    const value = params[name];
    if (typeof value === 'number') return formatNumber(value);
    return value === null || value === undefined ? '' : String(value);
  });
}

/**
 * The text of `key` in the language in use, with `params` put in its `{name}` places. With a
 * numeric `params.count`, the plural form of the language is chosen (`key.one`, `key.few`, …,
 * then `key.other`). Numbers are formatted for the language: pass a string for a value that must
 * not be grouped (coordinates, years, identifiers). A key in no dictionary gives the key itself.
 */
export function t(key, params) {
  const text = template(key, params);
  return text === undefined ? key : fill(text, params);
}

/** Whether a key (or its plural forms) is in the chosen dictionary or the English one. */
export function hasTranslation(key) {
  return template(key) !== undefined || template(`${key}.other`) !== undefined;
}

function params(el) {
  const raw = el.getAttribute('data-i18n-params');
  if (!raw) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}

function translate(el) {
  const values = params(el);
  const key = el.getAttribute('data-i18n');
  if (key) {
    const text = template(key, values);
    if (text !== undefined) {
      const filled = fill(text, values);
      if (el.textContent !== filled) el.textContent = filled;
    }
  }
  for (const [data, attribute] of ATTRIBUTES) {
    const name = el.getAttribute(data);
    if (!name) continue;
    const text = template(name, values);
    if (text === undefined) continue;
    const filled = fill(text, values);
    if (el.getAttribute(attribute) !== filled) el.setAttribute(attribute, filled);
  }
}

/**
 * Fills the `data-i18n*` elements of `root` (an element, a fragment or the document), `root`
 * itself included. An element whose key is in no dictionary keeps its text.
 */
export function applyTranslations(root = document) {
  if (!root) return;
  if (root.nodeType === 1 && root.matches(SELECTOR)) translate(root);
  for (const el of root.querySelectorAll(SELECTOR)) translate(el);
}

/**
 * A number as the language in use writes it. `options`: a count of decimals (as many shown
 * as asked, `formatNumber(2.5, 2)` → "2.50"), or the options of `Intl.NumberFormat`.
 */
export function formatNumber(value, options) {
  const settings = typeof options === 'number' ? { minimumFractionDigits: options, maximumFractionDigits: options } : options || {};
  const id = `${current} ${JSON.stringify(settings)}`;
  let format = numberFormats.get(id);
  if (!format) {
    format = new Intl.NumberFormat(current, settings);
    numberFormats.set(id, format);
  }
  return format.format(value);
}

const BYTE_UNITS = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte'];

/**
 * A size in bytes as the language writes it ("6 GB", "17.5 GB", "6 Go"). The steps are of 1024,
 * as Windows counts memory, so `formatBytes(gb * 1024 ** 3)` shows `gb` GB. `unit` forces one
 * of `byte`, `kilobyte`, `megabyte`, `gigabyte`, `terabyte` (else the largest that keeps the
 * value at 1 or more); `digits` fixes the count of decimals (else up to one, none when whole).
 */
export function formatBytes(bytes, { unit, digits } = {}) {
  let step = BYTE_UNITS.indexOf(unit);
  if (step < 0) {
    step = 0;
    while (step < BYTE_UNITS.length - 1 && Math.abs(bytes) >= 1024 ** (step + 1)) step += 1;
  }
  const decimals = typeof digits === 'number'
    ? { minimumFractionDigits: digits, maximumFractionDigits: digits }
    : { minimumFractionDigits: 0, maximumFractionDigits: step === 0 ? 0 : 1 };
  return formatNumber(bytes / 1024 ** step, { style: 'unit', unit: BYTE_UNITS[step], unitDisplay: 'short', ...decimals });
}

// a browser whose languages change follows them, unless the page chose one
if (typeof window !== 'undefined') {
  window.addEventListener('languagechange', () => {
    if (automatic) setLanguage(null);
  });
}

/**
 * Resolves (to the code in use) once the dictionaries of the browser's language are loaded and
 * the static text of the page is filled. It never rejects: a dictionary that cannot be read
 * leaves the English text.
 */
export const ready = setLanguage(null);
