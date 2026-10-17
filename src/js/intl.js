// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript Intl — ECMA-402 for engines that ship without ICU.
// Formatting data is English (en-US); time zones (tz database), collation and
// text segmentation come from native services (intl.rs). An engine with its
// own Intl (V8) skips this file entirely.
(function (global) {
  'use strict';
  if (typeof global.Intl !== 'undefined') return;
  const N = global.__n;
  const DEFAULT_LOCALE = 'en-US';
  const NBSP = ' ', NNBSP = ' ', THIN = ' ';

  // ------------------------------------------------------------ helpers
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true });
  function setTag(C, tag) { Object.defineProperty(C.prototype, Symbol.toStringTag, { value: tag, configurable: true }); }
  function toOptions(o) {
    if (o === undefined) return Object.create(null);
    if (o === null) throw new TypeError('Cannot convert undefined or null to object');
    return Object(o);
  }
  function getOpt(o, k, allowed, def) {
    let v = o[k];
    if (v === undefined) return def;
    if (typeof v === 'symbol') throw new TypeError('Cannot convert a Symbol value to a string');
    v = String(v);
    if (allowed && !allowed.includes(v)) throw new RangeError(`Value ${v} out of range for Intl.${this || 'Object'} options property ${k}`);
    return v;
  }
  function getBool(o, k, def) { const v = o[k]; return v === undefined ? def : Boolean(v); }
  function getNum(o, k, min, max, def) {
    let v = o[k];
    if (v === undefined) return def;
    v = Number(v);
    if (isNaN(v) || v < min || v > max) throw new RangeError(`${k} value is out of range.`);
    return Math.floor(v);
  }
  function brand(o, slot, name, method) {
    if (o === null || typeof o !== 'object' || !o[slot]) throw new TypeError(`Method Intl.${name}.prototype.${method} called on incompatible receiver ${o}`);
    return o[slot];
  }
  // Constructors that may be called without `new` (NumberFormat, DateTimeFormat, Collator).
  function callable(C) {
    const F = function (...a) { return new C(...a); };
    F.prototype = C.prototype;
    Object.defineProperty(C.prototype, 'constructor', { value: F, configurable: true, writable: true });
    Object.defineProperty(F, 'name', { value: C.name });
    Object.defineProperty(F, 'length', { value: 0 });
    for (const k of Object.getOwnPropertyNames(C)) if (!['length', 'name', 'prototype'].includes(k)) F[k] = C[k];
    return F;
  }

  // ------------------------------------------------------------ locales
  function canonTag(tag) {
    if (tag instanceof Locale) return tag.toString();
    if (typeof tag !== 'string' && (typeof tag !== 'object' || tag === null)) throw new TypeError('Language ID should be string or object.');
    const s = String(tag);
    if (!/^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$/.test(s)) throw new RangeError('Incorrect locale information provided');
    const parts = s.split('-');
    const out = [parts[0].toLowerCase()];
    let ext = false;
    for (let i = 1; i < parts.length; i++) {
      const p = parts[i];
      if (ext || p.length === 1) { ext = true; out.push(p.toLowerCase()); continue; }
      if (i === 1 && p.length === 4 && /^[A-Za-z]{4}$/.test(p)) out.push(p[0].toUpperCase() + p.slice(1).toLowerCase());
      else if (/^[A-Za-z]{2}$/.test(p) || /^\d{3}$/.test(p)) out.push(p.toUpperCase());
      else out.push(p.toLowerCase());
    }
    const ALIAS = { iw: 'he', in: 'id', ji: 'yi', jw: 'jv', mo: 'ro', tl: 'fil' };
    if (ALIAS[out[0]]) out[0] = ALIAS[out[0]];
    return out.join('-');
  }
  function canonList(locales) {
    if (locales === undefined) return [];
    const list = typeof locales === 'string' || locales instanceof Locale ? [locales] : Array.from(Object(locales));
    const seen = [];
    for (const l of list) { const c = canonTag(l); if (!seen.includes(c)) seen.push(c); }
    return seen;
  }
  // Split a tag into its base name and Unicode extension keywords.
  function splitTag(tag) {
    const parts = tag.split('-'); const base = []; const kw = {};
    let i = 0;
    while (i < parts.length && parts[i].length !== 1) base.push(parts[i++]);
    while (i < parts.length) {
      if (parts[i] === 'u') {
        i++; let key = null;
        while (i < parts.length && parts[i].length !== 1) { const p = parts[i++]; if (p.length === 2) { key = p; kw[key] = ''; } else if (key) kw[key] = kw[key] ? kw[key] + '-' + p : p; }
      } else if (parts[i] === 'x') break;
      else { i++; while (i < parts.length && parts[i].length !== 1) i++; }
    }
    return { base: base.join('-'), kw };
  }
  function resolveLocale(locales) {
    const list = canonList(locales);
    const { base, kw } = splitTag(list.length ? list[0] : DEFAULT_LOCALE);
    return { locale: base, kw, lang: base.split('-')[0] };
  }
  function supportedLocalesOf(locales, options) {
    const o = toOptions(options);
    getOpt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    return canonList(locales);
  }

  // ------------------------------------------------------------ decimals
  // A non-negative decimal: value = 0.d × 10^e (d: digits without leading or
  // trailing zeros; '' for zero). Formatting rounds on the shortest decimal
  // representation, like ICU.
  function norm(digits, e) {
    let i = 0;
    while (i < digits.length && digits[i] === '0') { i++; e--; }
    digits = digits.slice(i).replace(/0+$/, '');
    return digits ? { d: digits, e } : { d: '', e: 0 };
  }
  function decOfString(s) {
    let exp = 0; const ei = s.search(/e/i);
    if (ei >= 0) { exp = Number(s.slice(ei + 1)); s = s.slice(0, ei); }
    const dot = s.indexOf('.');
    return dot >= 0 ? norm(s.slice(0, dot) + s.slice(dot + 1), dot + exp) : norm(s, s.length + exp);
  }
  const shift = (x, n) => (x.d ? { d: x.d, e: x.e + n } : x);
  function roundAt(x, p, mode, neg) {
    if (!x.d) return x;
    const k = x.e - p;
    if (k >= x.d.length) return x;
    const kept = k > 0 ? x.d.slice(0, k) : '';
    const rest = k >= 0 ? x.d.slice(k) : '0'.repeat(-k) + x.d;
    const first = rest.charCodeAt(0) - 48, tail = /[1-9]/.test(rest.slice(1));
    const half = first > 5 || (first === 5 && tail) ? 1 : first === 5 ? 0 : -1;
    const nonzero = first > 0 || tail;
    const odd = kept ? (kept.charCodeAt(kept.length - 1) - 48) % 2 === 1 : false;
    let up;
    switch (mode) {
      case 'ceil': up = !neg && nonzero; break;
      case 'floor': up = neg && nonzero; break;
      case 'expand': up = nonzero; break;
      case 'trunc': up = false; break;
      case 'halfCeil': up = half > 0 || (half === 0 && !neg); break;
      case 'halfFloor': up = half > 0 || (half === 0 && neg); break;
      case 'halfTrunc': up = half > 0; break;
      case 'halfEven': up = half > 0 || (half === 0 && odd); break;
      default: up = half >= 0;
    }
    if (!up) return k > 0 ? norm(kept, x.e) : { d: '', e: 0 };
    if (k <= 0) return { d: '1', e: p + 1 };
    const a = kept.split(''); let i = a.length - 1;
    while (i >= 0 && a[i] === '9') a[i--] = '0';
    if (i < 0) return norm('1' + a.join(''), x.e + 1);
    a[i] = String.fromCharCode(a[i].charCodeAt(0) + 1);
    return norm(a.join(''), x.e);
  }
  function decToStrings(x, minFrac) {
    let int, frac;
    if (!x.d) { int = '0'; frac = ''; }
    else if (x.e <= 0) { int = '0'; frac = '0'.repeat(-x.e) + x.d; }
    else if (x.e >= x.d.length) { int = x.d + '0'.repeat(x.e - x.d.length); frac = ''; }
    else { int = x.d.slice(0, x.e); frac = x.d.slice(x.e); }
    if (frac.length < minFrac) frac += '0'.repeat(minFrac - frac.length);
    return { int, frac };
  }
  // Mathematical value of a format() argument: {neg, x} or {neg, special}.
  function toIntlValue(v) {
    if (typeof v === 'bigint') { const neg = v < 0n; return { neg, x: decOfString((neg ? -v : v).toString()) }; }
    if (typeof v === 'string') {
      const m = /^\s*([+-])?((?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)\s*$/.exec(v);
      if (m) { const x = decOfString(m[2]); return { neg: m[1] === '-', x }; }
    }
    const n = Number(v);
    if (isNaN(n)) return { neg: false, special: 'nan' };
    const neg = n < 0 || Object.is(n, -0);
    if (!isFinite(n)) return { neg, special: 'infinity' };
    return { neg, x: n === 0 ? { d: '', e: 0 } : decOfString(String(Math.abs(n))) };
  }

  // Digit options (SetNumberFormatDigitOptions).
  function digitOptions(r, o, mnfdDefault, mxfdDefault, notation) {
    const mnid = getNum(o, 'minimumIntegerDigits', 1, 21, 1);
    let mnfd = o.minimumFractionDigits, mxfd = o.maximumFractionDigits, mnsd = o.minimumSignificantDigits, mxsd = o.maximumSignificantDigits;
    r.mnid = mnid;
    const inc = getNum(o, 'roundingIncrement', 1, 5000, 1);
    if (![1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000].includes(inc)) throw new RangeError('roundingIncrement value is out of range.');
    const mode = getOpt.call('NumberFormat', o, 'roundingMode', ['ceil', 'floor', 'expand', 'trunc', 'halfCeil', 'halfFloor', 'halfExpand', 'halfTrunc', 'halfEven'], 'halfExpand');
    const prio = getOpt.call('NumberFormat', o, 'roundingPriority', ['auto', 'morePrecision', 'lessPrecision'], 'auto');
    const tzd = getOpt.call('NumberFormat', o, 'trailingZeroDisplay', ['auto', 'stripIfInteger'], 'auto');
    if (inc !== 1) mxfdDefault = mnfdDefault;
    r.inc = inc; r.roundingMode = mode; r.trailingZeroDisplay = tzd;
    const hasSd = mnsd !== undefined || mxsd !== undefined, hasFd = mnfd !== undefined || mxfd !== undefined;
    let needSd = true, needFd = true;
    if (prio === 'auto') { needSd = hasSd; if (needSd || (!hasFd && notation === 'compact')) needFd = false; }
    if (needSd) {
      if (hasSd) {
        r.mnsd = getNum(o, 'minimumSignificantDigits', 1, 21, 1);
        r.mxsd = getNum(o, 'maximumSignificantDigits', r.mnsd, 21, 21);
      } else { r.mnsd = 1; r.mxsd = 21; }
    }
    if (needFd) {
      if (hasFd) {
        mnfd = getNum(o, 'minimumFractionDigits', 0, 100, undefined);
        mxfd = getNum(o, 'maximumFractionDigits', 0, 100, undefined);
        if (mnfd === undefined) mnfd = Math.min(mnfdDefault, mxfd);
        else if (mxfd === undefined) mxfd = Math.max(mxfdDefault, mnfd);
        else if (mnfd > mxfd) throw new RangeError('maximumFractionDigits value is out of range.');
        r.mnfd = mnfd; r.mxfd = mxfd;
      } else { r.mnfd = mnfdDefault; r.mxfd = mxfdDefault; }
    }
    if (!needSd && !needFd) { r.mnfd = 0; r.mxfd = 0; r.mnsd = 1; r.mxsd = 2; r.roundingType = 'morePrecision'; r.roundingPriority = 'morePrecision'; }
    else if (prio === 'auto') { r.roundingType = hasSd ? 'significantDigits' : 'fractionDigits'; r.roundingPriority = 'auto'; }
    else { r.roundingType = prio; r.roundingPriority = prio; }
    if (inc !== 1 && (r.roundingType !== 'fractionDigits' || r.mxfd !== r.mnfd)) throw new RangeError('maximumFractionDigits value is out of range.');
  }
  // FormatNumericToString: rounds `x` per the digit options -> {int, frac, y}.
  function formatDigits(r, x, neg) {
    const sd = () => {
      const p = (x.d ? x.e : 1) - r.mxsd;
      const y = roundAt(x, p, r.roundingMode, neg);
      return { y, rm: p, minFrac: Math.max(0, r.mnsd - (y.d ? y.e : 1)) };
    };
    const fd = () => {
      let y;
      if (r.inc !== 1) {
        // Round to a multiple of inc × 10^-mxfd.
        const scaled = shift(x, r.mxfd);
        const q = Number(decToStrings(scaled, 0).int + '.' + decToStrings(scaled, 0).frac) / r.inc;
        const qd = roundAt(q === 0 ? { d: '', e: 0 } : decOfString(String(q)), 0, r.roundingMode, neg);
        y = shift(qd.d ? decOfString(String(Number(decToStrings(qd, 0).int) * r.inc)) : qd, -r.mxfd);
      } else y = roundAt(x, -r.mxfd, r.roundingMode, neg);
      return { y, rm: -r.mxfd, minFrac: r.mnfd };
    };
    let res;
    if (r.roundingType === 'significantDigits') res = sd();
    else if (r.roundingType === 'fractionDigits') res = fd();
    else { const s = sd(), f = fd(); res = r.roundingType === 'morePrecision' ? (s.rm <= f.rm ? s : f) : (s.rm <= f.rm ? f : s); }
    let { int, frac } = decToStrings(res.y, res.minFrac);
    if (r.trailingZeroDisplay === 'stripIfInteger' && /^0*$/.test(frac)) frac = '';
    if (int.length < r.mnid) int = '0'.repeat(r.mnid - int.length) + int;
    return { int, frac, y: res.y };
  }
  // English cardinal and ordinal plural rules on formatted digits.
  function pluralOf(int, frac, ordinal) {
    if (ordinal) {
      const n = Number(int.slice(-2)), m10 = n % 10;
      if (frac) return 'other';
      if (m10 === 1 && n !== 11) return 'one';
      if (m10 === 2 && n !== 12) return 'two';
      if (m10 === 3 && n !== 13) return 'few';
      return 'other';
    }
    return int.replace(/^0+(?=\d)/, '') === '1' && frac === '' ? 'one' : 'other';
  }

  // ------------------------------------------------------------ NumberFormat data
  // code: [symbol, narrow symbol, display name, singular, plural, digits]
  const CURRENCIES = {
    USD: ['$', '$', 'US Dollar', 'US dollar', 'US dollars'], EUR: ['€', '€', 'Euro', 'euro', 'euros'], GBP: ['£', '£', 'British Pound', 'British pound', 'British pounds'],
    JPY: ['¥', '¥', 'Japanese Yen', 'Japanese yen', 'Japanese yen', 0], CNY: ['CN¥', '¥', 'Chinese Yuan', 'Chinese yuan', 'Chinese yuan'], INR: ['₹', '₹', 'Indian Rupee', 'Indian rupee', 'Indian rupees'],
    KRW: ['₩', '₩', 'South Korean Won', 'South Korean won', 'South Korean won', 0], CAD: ['CA$', '$', 'Canadian Dollar', 'Canadian dollar', 'Canadian dollars'],
    AUD: ['A$', '$', 'Australian Dollar', 'Australian dollar', 'Australian dollars'], NZD: ['NZ$', '$', 'New Zealand Dollar', 'New Zealand dollar', 'New Zealand dollars'],
    HKD: ['HK$', '$', 'Hong Kong Dollar', 'Hong Kong dollar', 'Hong Kong dollars'], TWD: ['NT$', '$', 'New Taiwan Dollar', 'New Taiwan dollar', 'New Taiwan dollars'],
    MXN: ['MX$', '$', 'Mexican Peso', 'Mexican peso', 'Mexican pesos'], BRL: ['R$', 'R$', 'Brazilian Real', 'Brazilian real', 'Brazilian reals'],
    CHF: ['CHF', 'CHF', 'Swiss Franc', 'Swiss franc', 'Swiss francs'], RUB: ['RUB', '₽', 'Russian Ruble', 'Russian ruble', 'Russian rubles'],
    SEK: ['SEK', 'kr', 'Swedish Krona', 'Swedish krona', 'Swedish kronor'], NOK: ['NOK', 'kr', 'Norwegian Krone', 'Norwegian krone', 'Norwegian kroner'],
    DKK: ['DKK', 'kr', 'Danish Krone', 'Danish krone', 'Danish kroner'], PLN: ['PLN', 'zł', 'Polish Zloty', 'Polish zloty', 'Polish zlotys'],
    CZK: ['CZK', 'Kč', 'Czech Koruna', 'Czech koruna', 'Czech korunas'], HUF: ['HUF', 'Ft', 'Hungarian Forint', 'Hungarian forint', 'Hungarian forints'],
    TRY: ['TRY', '₺', 'Turkish Lira', 'Turkish lira', 'Turkish Lira'], ILS: ['₪', '₪', 'Israeli New Shekel', 'Israeli new shekel', 'Israeli new shekels'],
    VND: ['₫', '₫', 'Vietnamese Dong', 'Vietnamese dong', 'Vietnamese dong', 0], PHP: ['₱', '₱', 'Philippine Peso', 'Philippine peso', 'Philippine pesos'],
    THB: ['THB', '฿', 'Thai Baht', 'Thai baht', 'Thai baht'], SGD: ['SGD', '$', 'Singapore Dollar', 'Singapore dollar', 'Singapore dollars'],
    ZAR: ['ZAR', 'R', 'South African Rand', 'South African rand', 'South African rand'], UAH: ['UAH', '₴', 'Ukrainian Hryvnia', 'Ukrainian hryvnia', 'Ukrainian hryvnias'],
    NGN: ['NGN', '₦', 'Nigerian Naira', 'Nigerian naira', 'Nigerian nairas'], IDR: ['IDR', 'Rp', 'Indonesian Rupiah', 'Indonesian rupiah', 'Indonesian rupiahs'],
    MYR: ['MYR', 'RM', 'Malaysian Ringgit', 'Malaysian ringgit', 'Malaysian ringgits'], SAR: ['SAR', 'SAR', 'Saudi Riyal', 'Saudi riyal', 'Saudi riyals'],
    AED: ['AED', 'AED', 'United Arab Emirates Dirham', 'UAE dirham', 'UAE dirhams'], EGP: ['EGP', 'E£', 'Egyptian Pound', 'Egyptian pound', 'Egyptian pounds'],
    ARS: ['ARS', '$', 'Argentine Peso', 'Argentine peso', 'Argentine pesos'], CLP: ['CLP', '$', 'Chilean Peso', 'Chilean peso', 'Chilean pesos', 0],
    COP: ['COP', '$', 'Colombian Peso', 'Colombian peso', 'Colombian pesos'], PKR: ['PKR', 'Rs', 'Pakistani Rupee', 'Pakistani rupee', 'Pakistani rupees'],
    BDT: ['BDT', '৳', 'Bangladeshi Taka', 'Bangladeshi taka', 'Bangladeshi takas'], KES: ['KES', 'KES', 'Kenyan Shilling', 'Kenyan shilling', 'Kenyan shillings'],
    ISK: ['ISK', 'kr', 'Icelandic Króna', 'Icelandic króna', 'Icelandic krónur', 0], RON: ['RON', 'lei', 'Romanian Leu', 'Romanian leu', 'Romanian lei'],
    KWD: ['KWD', 'KWD', 'Kuwaiti Dinar', 'Kuwaiti dinar', 'Kuwaiti dinars', 3], BHD: ['BHD', 'BHD', 'Bahraini Dinar', 'Bahraini dinar', 'Bahraini dinars', 3],
    JOD: ['JOD', 'JOD', 'Jordanian Dinar', 'Jordanian dinar', 'Jordanian dinars', 3], OMR: ['OMR', 'OMR', 'Omani Rial', 'Omani rial', 'Omani rials', 3],
    XAF: ['FCFA', 'FCFA', 'Central African CFA Franc', 'Central African CFA franc', 'Central African CFA francs', 0],
    XOF: ['F CFA', 'F CFA', 'West African CFA Franc', 'West African CFA franc', 'West African CFA francs', 0],
    BTC: ['BTC', 'BTC', 'BTC', 'BTC', 'BTC'],
  };
  const ZERO_DIGIT = ['UGX', 'PYG', 'KMF', 'RWF', 'BIF', 'DJF', 'GNF', 'VUV', 'XPF'], THREE_DIGIT = ['IQD', 'LYD', 'TND'];
  function currencyDigits(c) {
    const e = CURRENCIES[c];
    if (e && e[5] !== undefined) return e[5];
    return ZERO_DIGIT.includes(c) ? 0 : THREE_DIGIT.includes(c) ? 3 : 2;
  }
  // unit: [short singular, short plural, narrow, long singular, long plural, short pattern has no space]
  const UNITS = {
    acre: ['ac', 'ac', 'ac', 'acre', 'acres'], bit: ['bit', 'bit', 'bit', 'bit', 'bits'], byte: ['byte', 'byte', 'B', 'byte', 'bytes'],
    celsius: ['°C', '°C', '°C', 'degree Celsius', 'degrees Celsius', 1], centimeter: ['cm', 'cm', 'cm', 'centimeter', 'centimeters'],
    day: ['day', 'days', 'd', 'day', 'days'], degree: ['deg', 'deg', '°', 'degree', 'degrees'], fahrenheit: ['°F', '°F', '°', 'degree Fahrenheit', 'degrees Fahrenheit', 1],
    'fluid-ounce': ['fl oz', 'fl oz', 'fl oz', 'fluid ounce', 'fluid ounces'], foot: ['ft', 'ft', '′', 'foot', 'feet'], gallon: ['gal', 'gal', 'gal', 'gallon', 'gallons'],
    gigabit: ['Gb', 'Gb', 'Gb', 'gigabit', 'gigabits'], gigabyte: ['GB', 'GB', 'GB', 'gigabyte', 'gigabytes'], gram: ['g', 'g', 'g', 'gram', 'grams'],
    hectare: ['ha', 'ha', 'ha', 'hectare', 'hectares'], hour: ['hr', 'hr', 'h', 'hour', 'hours'], inch: ['in', 'in', '″', 'inch', 'inches'],
    kilobit: ['kb', 'kb', 'kb', 'kilobit', 'kilobits'], kilobyte: ['kB', 'kB', 'kB', 'kilobyte', 'kilobytes'], kilogram: ['kg', 'kg', 'kg', 'kilogram', 'kilograms'],
    kilometer: ['km', 'km', 'km', 'kilometer', 'kilometers'], liter: ['L', 'L', 'L', 'liter', 'liters'], megabit: ['Mb', 'Mb', 'Mb', 'megabit', 'megabits'],
    megabyte: ['MB', 'MB', 'MB', 'megabyte', 'megabytes'], meter: ['m', 'm', 'm', 'meter', 'meters'], microsecond: ['μs', 'μs', 'μs', 'microsecond', 'microseconds'],
    mile: ['mi', 'mi', 'mi', 'mile', 'miles'], 'mile-scandinavian': ['smi', 'smi', 'smi', 'mile-scandinavian', 'miles-scandinavian'],
    milliliter: ['mL', 'mL', 'mL', 'milliliter', 'milliliters'], millimeter: ['mm', 'mm', 'mm', 'millimeter', 'millimeters'],
    millisecond: ['ms', 'ms', 'ms', 'millisecond', 'milliseconds'], minute: ['min', 'min', 'm', 'minute', 'minutes'], month: ['mth', 'mths', 'm', 'month', 'months'],
    nanosecond: ['ns', 'ns', 'ns', 'nanosecond', 'nanoseconds'], ounce: ['oz', 'oz', 'oz', 'ounce', 'ounces'], percent: ['%', '%', '%', 'percent', 'percent', 1],
    petabyte: ['PB', 'PB', 'PB', 'petabyte', 'petabytes'], pound: ['lb', 'lb', 'lb', 'pound', 'pounds'], second: ['sec', 'sec', 's', 'second', 'seconds'],
    stone: ['st', 'st', 'st', 'stone', 'stones'], terabit: ['Tb', 'Tb', 'Tb', 'terabit', 'terabits'], terabyte: ['TB', 'TB', 'TB', 'terabyte', 'terabytes'],
    week: ['wk', 'wks', 'w', 'week', 'weeks'], yard: ['yd', 'yd', 'yd', 'yard', 'yards'], year: ['yr', 'yrs', 'y', 'year', 'years'],
  };
  const COMPOUND = {
    'kilometer-per-hour': ['km/h', 'km/h', 'km/h', 'kilometer per hour', 'kilometers per hour'], 'mile-per-hour': ['mph', 'mph', 'mph', 'mile per hour', 'miles per hour'],
    'meter-per-second': ['m/s', 'm/s', 'm/s', 'meter per second', 'meters per second'], 'mile-per-gallon': ['mpg', 'mpg', 'mpg', 'mile per gallon', 'miles per gallon'],
    'liter-per-kilometer': ['L/km', 'L/km', 'L/km', 'liter per kilometer', 'liters per kilometer'],
  };
  const PER = { second: '/s', minute: '/min', hour: '/h', day: '/d', week: '/w', month: '/m', year: '/y' };
  function unitData(u) {
    if (UNITS[u]) return UNITS[u];
    if (COMPOUND[u]) return COMPOUND[u];
    const m = /^([a-z-]+?)-per-([a-z-]+)$/.exec(u);
    if (m && UNITS[m[1]] && UNITS[m[2]]) {
      const a = UNITS[m[1]], b = UNITS[m[2]], per = PER[m[2]] || '/' + b[0];
      return [a[0] + per, a[1] + per, a[2] + per, a[3] + ' per ' + b[3], a[4] + ' per ' + b[3]];
    }
    return null;
  }
  const COMPACT_SHORT = { 3: 'K', 6: 'M', 9: 'B', 12: 'T' }, COMPACT_LONG = { 3: 'thousand', 6: 'million', 9: 'billion', 12: 'trillion' };

  // ------------------------------------------------------------ NumberFormat
  class NumberFormat {
    constructor(locales, options) {
      const { locale, kw } = resolveLocale(locales);
      const o = toOptions(options);
      const opt = getOpt.bind('NumberFormat');
      const r = { locale, numberingSystem: 'latn' };
      opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      const ns = opt(o, 'numberingSystem', null, kw.nu);
      if (ns !== undefined && !/^[a-z0-9]{3,8}(-[a-z0-9]{3,8})*$/i.test(ns)) throw new RangeError('Invalid numberingSystem : ' + ns);
      r.style = opt(o, 'style', ['decimal', 'percent', 'currency', 'unit'], 'decimal');
      let currency = opt(o, 'currency', null, undefined);
      if (currency !== undefined) { if (!/^[A-Za-z]{3}$/.test(currency)) throw new RangeError('Invalid currency code : ' + currency); currency = currency.toUpperCase(); }
      const currencyDisplay = opt(o, 'currencyDisplay', ['code', 'symbol', 'narrowSymbol', 'name'], 'symbol');
      const currencySign = opt(o, 'currencySign', ['standard', 'accounting'], 'standard');
      const unit = opt(o, 'unit', null, undefined);
      if (unit !== undefined && !unitData(unit)) throw new RangeError('Invalid unit argument for Intl.NumberFormat() \'' + unit + '\'');
      const unitDisplay = opt(o, 'unitDisplay', ['short', 'narrow', 'long'], 'short');
      if (r.style === 'currency') {
        if (currency === undefined) throw new TypeError('Currency code is required with currency style.');
        Object.assign(r, { currency, currencyDisplay, currencySign });
      }
      if (r.style === 'unit') {
        if (unit === undefined) throw new TypeError('Unit is required with unit style.');
        Object.assign(r, { unit, unitDisplay });
      }
      const notation = opt(o, 'notation', ['standard', 'scientific', 'engineering', 'compact'], 'standard');
      const cd = r.style === 'currency' && notation === 'standard' ? currencyDigits(currency) : 0;
      digitOptions(r, o, r.style === 'currency' && notation === 'standard' ? cd : 0, r.style === 'currency' && notation === 'standard' ? cd : r.style === 'percent' ? 0 : 3, notation);
      r.notation = notation;
      const compactDisplay = opt(o, 'compactDisplay', ['short', 'long'], 'short');
      const defGrouping = notation === 'compact' ? 'min2' : 'auto';
      let g = o.useGrouping;
      if (g === undefined) g = defGrouping;
      else if (g === true) g = 'always';
      else if (g === false || g === null || g === '' || g === 0) g = false;
      else { g = String(g); if (g === 'true' || g === 'false') g = defGrouping; else if (!['min2', 'auto', 'always'].includes(g)) throw new RangeError('Value ' + g + ' out of range for Intl.NumberFormat options property useGrouping'); }
      r.useGrouping = g;
      if (notation === 'compact') r.compactDisplay = compactDisplay;
      r.signDisplay = opt(o, 'signDisplay', ['auto', 'never', 'always', 'exceptZero', 'negative'], 'auto');
      hidden(this, '_nf', r);
    }
    get format() {
      const r = brand(this, '_nf', 'NumberFormat', 'format');
      if (!r.bound) r.bound = (v) => partsToString(numberParts(r, v));
      return r.bound;
    }
    formatToParts(v) { return numberParts(brand(this, '_nf', 'NumberFormat', 'formatToParts'), v); }
    formatRange(a, b) { return this.formatRangeToParts(a, b).map((p) => p.value).join(''); }
    formatRangeToParts(a, b) {
      const r = brand(this, '_nf', 'NumberFormat', 'formatRange');
      if (a === undefined || b === undefined) throw new TypeError('start or end is undefined');
      const pa = numberParts(r, a), pb = numberParts(r, b);
      if (partsToString(pa) === partsToString(pb)) return [{ type: 'approximatelySign', value: '~', source: 'shared' }].concat(pa.map((p) => Object.assign(p, { source: 'shared' })));
      const sep = r.style === 'decimal' && r.notation === 'standard' ? '–' : ' – ';
      return pa.map((p) => Object.assign(p, { source: 'startRange' })).concat([{ type: 'literal', value: sep, source: 'shared' }], pb.map((p) => Object.assign(p, { source: 'endRange' })));
    }
    resolvedOptions() {
      const r = brand(this, '_nf', 'NumberFormat', 'resolvedOptions');
      const out = { locale: r.locale, numberingSystem: r.numberingSystem, style: r.style };
      for (const k of ['currency', 'currencyDisplay', 'currencySign', 'unit', 'unitDisplay']) if (r[k] !== undefined) out[k] = r[k];
      out.minimumIntegerDigits = r.mnid;
      if (r.roundingType !== 'significantDigits') { out.minimumFractionDigits = r.mnfd; out.maximumFractionDigits = r.mxfd; }
      if (r.roundingType !== 'fractionDigits') { out.minimumSignificantDigits = r.mnsd; out.maximumSignificantDigits = r.mxsd; }
      out.useGrouping = r.useGrouping; out.notation = r.notation;
      if (r.compactDisplay) out.compactDisplay = r.compactDisplay;
      Object.assign(out, { signDisplay: r.signDisplay, roundingIncrement: r.inc, roundingMode: r.roundingMode, roundingPriority: r.roundingPriority, trailingZeroDisplay: r.trailingZeroDisplay });
      return out;
    }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(NumberFormat, 'Intl.NumberFormat');
  const partsToString = (parts) => parts.map((p) => p.value).join('');

  function computeExponent(r, x, neg) {
    if (!x.d) return 0;
    const forMag = (m) => (r.notation === 'scientific' ? m : r.notation === 'engineering' ? Math.floor(m / 3) * 3 : m < 3 ? 0 : Math.min(Math.floor(m / 3) * 3, 12));
    const mag = x.e - 1;
    const exp = forMag(mag);
    const y = formatDigits(r, shift(x, -exp), neg).y;
    if (!y.d || y.e - 1 === mag - exp) return exp;
    return forMag(mag + 1);
  }
  function numberParts(r, v) {
    const val = toIntlValue(v);
    let x = val.x;
    const num = [];
    let int = '', frac = '', isZero = false, exp = 0;
    if (val.special) {
      num.push(val.special === 'nan' ? { type: 'nan', value: 'NaN' } : { type: 'infinity', value: '∞' });
    } else {
      if (r.style === 'percent') x = shift(x, 2);
      if (r.notation !== 'standard') { exp = computeExponent(r, x, val.neg); x = shift(x, -exp); }
      const f = formatDigits(r, x, val.neg);
      int = f.int; frac = f.frac; isZero = !f.y.d;
      const g = r.useGrouping;
      const group = (g === 'always' || g === 'auto') ? int.length > 3 : g === 'min2' ? int.length > 4 : false;
      if (group) {
        const first = int.length % 3 || 3;
        num.push({ type: 'integer', value: int.slice(0, first) });
        for (let i = first; i < int.length; i += 3) { num.push({ type: 'group', value: ',' }); num.push({ type: 'integer', value: int.slice(i, i + 3) }); }
      } else num.push({ type: 'integer', value: int });
      if (frac) { num.push({ type: 'decimal', value: '.' }); num.push({ type: 'fraction', value: frac }); }
      if (r.notation === 'compact' && exp) {
        if (r.compactDisplay === 'long') { num.push({ type: 'literal', value: ' ' }); num.push({ type: 'compact', value: COMPACT_LONG[exp] }); }
        else num.push({ type: 'compact', value: COMPACT_SHORT[exp] });
      } else if (r.notation === 'scientific' || r.notation === 'engineering') {
        num.push({ type: 'exponentSeparator', value: 'E' });
        if (exp < 0) num.push({ type: 'exponentMinusSign', value: '-' });
        num.push({ type: 'exponentInteger', value: String(Math.abs(exp)) });
      }
    }
    const neg = val.neg && val.special !== 'nan';
    let sign = '';
    switch (r.signDisplay) {
      case 'auto': sign = neg ? '-' : ''; break;
      case 'always': sign = neg ? '-' : '+'; break;
      case 'exceptZero': sign = isZero || val.special === 'nan' ? '' : neg ? '-' : '+'; break;
      case 'negative': sign = neg && !isZero ? '-' : ''; break;
      default: sign = '';
    }
    const signParts = sign === '-' ? [{ type: 'minusSign', value: '-' }] : sign === '+' ? [{ type: 'plusSign', value: '+' }] : [];
    const plural = () => (exp ? 'other' : pluralOf(int || '0', frac, false));
    if (r.style === 'percent') return signParts.concat(num, [{ type: 'percentSign', value: '%' }]);
    if (r.style === 'unit') {
      const u = unitData(r.unit);
      if (r.unitDisplay === 'narrow') return signParts.concat(num, [{ type: 'unit', value: u[2] }]);
      if (r.unitDisplay === 'long') return signParts.concat(num, [{ type: 'literal', value: ' ' }, { type: 'unit', value: plural() === 'one' ? u[3] : u[4] }]);
      const s = plural() === 'one' ? u[0] : u[1];
      return signParts.concat(num, u[5] ? [{ type: 'unit', value: s }] : [{ type: 'literal', value: ' ' }, { type: 'unit', value: s }]);
    }
    if (r.style === 'currency') {
      const c = CURRENCIES[r.currency];
      if (r.currencyDisplay === 'name') {
        const name = c ? (plural() === 'one' ? c[3] : c[4]) : r.currency;
        return signParts.concat(num, [{ type: 'literal', value: ' ' }, { type: 'currency', value: name }]);
      }
      const sym = r.currencyDisplay === 'code' ? r.currency : c ? (r.currencyDisplay === 'narrowSymbol' ? c[1] : c[0]) : r.currency;
      const cur = [{ type: 'currency', value: sym }];
      if (/\p{L}$/u.test(sym)) cur.push({ type: 'literal', value: NBSP });
      if (r.currencySign === 'accounting' && sign === '-') return [{ type: 'literal', value: '(' }].concat(cur, num, [{ type: 'literal', value: ')' }]);
      return signParts.concat(cur, num);
    }
    return signParts.concat(num);
  }

  // ------------------------------------------------------------ PluralRules
  class PluralRules {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.PluralRules requires 'new'");
      const { locale, lang } = resolveLocale(locales);
      const o = toOptions(options);
      getOpt.call('PluralRules', o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      const r = { locale: lang === 'en' ? 'en' : locale, type: getOpt.call('PluralRules', o, 'type', ['cardinal', 'ordinal'], 'cardinal') };
      const notation = getOpt.call('PluralRules', o, 'notation', ['standard', 'scientific', 'engineering', 'compact'], 'standard');
      digitOptions(r, o, 0, 3, notation);
      r.notation = notation;
      hidden(this, '_pr', r);
    }
    select(n) {
      const r = brand(this, '_pr', 'PluralRules', 'select');
      const v = toIntlValue(Number(n));
      if (v.special) return 'other';
      const f = formatDigits(r, v.x, v.neg);
      return pluralOf(f.int, f.frac, r.type === 'ordinal');
    }
    selectRange(a, b) {
      brand(this, '_pr', 'PluralRules', 'selectRange');
      if (a === undefined || b === undefined) throw new TypeError('start or end is undefined');
      if (isNaN(Number(a)) || isNaN(Number(b))) throw new RangeError('Invalid range');
      return this.select(b) === 'one' && this.select(a) === 'one' ? 'one' : 'other';
    }
    resolvedOptions() {
      const r = brand(this, '_pr', 'PluralRules', 'resolvedOptions');
      const out = { locale: r.locale, type: r.type, minimumIntegerDigits: r.mnid };
      if (r.roundingType !== 'significantDigits') { out.minimumFractionDigits = r.mnfd; out.maximumFractionDigits = r.mxfd; }
      if (r.roundingType !== 'fractionDigits') { out.minimumSignificantDigits = r.mnsd; out.maximumSignificantDigits = r.mxsd; }
      out.pluralCategories = r.type === 'ordinal' ? ['one', 'two', 'few', 'other'] : ['one', 'other'];
      return Object.assign(out, { roundingIncrement: r.inc, roundingMode: r.roundingMode, roundingPriority: r.roundingPriority, trailingZeroDisplay: r.trailingZeroDisplay });
    }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(PluralRules, 'Intl.PluralRules');

  // ------------------------------------------------------------ RelativeTimeFormat
  const RT_UNITS = ['second', 'minute', 'hour', 'day', 'week', 'month', 'quarter', 'year'];
  // unit: [short singular, short plural, narrow]
  const RT_SHORT = { second: ['sec.', 'sec.', 's'], minute: ['min.', 'min.', 'm'], hour: ['hr.', 'hr.', 'h'], day: ['day', 'days', 'd'], week: ['wk.', 'wk.', 'w'], month: ['mo.', 'mo.', 'mo'], quarter: ['qtr.', 'qtrs.', 'q'], year: ['yr.', 'yr.', 'y'] };
  const RT_AUTO = {
    day: { '-1': 'yesterday', 0: 'today', 1: 'tomorrow' }, second: { 0: 'now' }, minute: { 0: 'this minute' }, hour: { 0: 'this hour' },
  };
  class RelativeTimeFormat {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.RelativeTimeFormat requires 'new'");
      const { locale, kw } = resolveLocale(locales);
      const o = toOptions(options);
      const opt = getOpt.bind('RelativeTimeFormat');
      opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      opt(o, 'numberingSystem', null, kw.nu);
      const r = { locale, style: opt(o, 'style', ['long', 'short', 'narrow'], 'long'), numeric: opt(o, 'numeric', ['always', 'auto'], 'always'), numberingSystem: 'latn' };
      r.nf = new NumberFormat(locale)._nf;
      hidden(this, '_rt', r);
    }
    format(v, unit) { return this.formatToParts(v, unit).map((p) => p.value).join(''); }
    formatToParts(v, unit) {
      const r = brand(this, '_rt', 'RelativeTimeFormat', 'format');
      v = Number(v);
      if (!isFinite(v)) throw new RangeError('Invalid value ' + v + ' in Intl.RelativeTimeFormat.prototype.format');
      const u = String(unit).replace(/s$/, '');
      if (!RT_UNITS.includes(u)) throw new RangeError('Invalid unit argument for format() \'' + unit + '\'');
      if (r.numeric === 'auto') {
        const key = Object.is(v, -0) ? '0' : String(v);
        let phrase = RT_AUTO[u] && RT_AUTO[u][key];
        if (!phrase && ['week', 'month', 'quarter', 'year'].includes(u) && (v === -1 || v === 0 || v === 1)) {
          const name = r.style === 'long' ? u : RT_SHORT[u][0];
          phrase = (v === -1 ? 'last ' : v === 0 ? 'this ' : 'next ') + name;
        }
        if (phrase) return [{ type: 'literal', value: phrase }];
      }
      const past = v < 0 || Object.is(v, -0);
      const nparts = numberParts(r.nf, Math.abs(v));
      const plural = pluralOf(nparts.filter((p) => p.type === 'integer').map((p) => p.value).join(''), (nparts.find((p) => p.type === 'fraction') || { value: '' }).value, false);
      let name;
      if (r.style === 'long') name = ' ' + u + (plural === 'one' ? '' : 's');
      else if (r.style === 'short') name = ' ' + RT_SHORT[u][plural === 'one' ? 0 : 1];
      else name = RT_SHORT[u][2];
      const num = nparts.map((p) => Object.assign(p, { unit: u }));
      return past ? num.concat([{ type: 'literal', value: name + ' ago' }]) : [{ type: 'literal', value: 'in ' }].concat(num, [{ type: 'literal', value: name }]);
    }
    resolvedOptions() {
      const r = brand(this, '_rt', 'RelativeTimeFormat', 'resolvedOptions');
      return { locale: r.locale, style: r.style, numeric: r.numeric, numberingSystem: r.numberingSystem };
    }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(RelativeTimeFormat, 'Intl.RelativeTimeFormat');

  // ------------------------------------------------------------ time zones
  const tzCache = new Map();
  // ICU's canonical ids (the older names) for renamed zones.
  const ICU_ZONE = {
    'Asia/Kolkata': 'Asia/Calcutta', 'Asia/Kathmandu': 'Asia/Katmandu', 'Asia/Ho_Chi_Minh': 'Asia/Saigon', 'Asia/Yangon': 'Asia/Rangoon', 'Europe/Kyiv': 'Europe/Kiev',
    'America/Nuuk': 'America/Godthab', 'Atlantic/Faroe': 'Atlantic/Faeroe', 'Pacific/Kanton': 'Pacific/Enderbury', 'Pacific/Chuuk': 'Pacific/Truk', 'Pacific/Pohnpei': 'Pacific/Ponape',
    'America/Argentina/Buenos_Aires': 'America/Buenos_Aires', 'America/Indiana/Indianapolis': 'America/Indianapolis', 'America/Kentucky/Louisville': 'America/Louisville',
    'America/Argentina/Cordoba': 'America/Cordoba', 'Asia/Dhaka': 'Asia/Dacca', 'Asia/Thimphu': 'Asia/Thimbu', 'Asia/Ulaanbaatar': 'Asia/Ulan_Bator', 'Asia/Macau': 'Asia/Macao',
    'Atlantic/Reykjavik': 'Atlantic/Reykjavik', 'Europe/Uzhgorod': 'Europe/Uzhgorod',
  };
  function tzInfo(tz, t) {
    // Offsets change at most a few times a year; cache per zone and hour.
    const key = tz + '@' + Math.floor(t / 3600000);
    let v = tzCache.get(key);
    if (!v) {
      v = N.tzOffset(tz, t) || [0, 'UTC', false];
      if (tzCache.size > 512) tzCache.clear();
      tzCache.set(key, v);
    }
    return v;
  }
  const US_ZONES = { EST: 'Eastern', EDT: 'Eastern', CST: 'Central', CDT: 'Central', MST: 'Mountain', MDT: 'Mountain', PST: 'Pacific', PDT: 'Pacific', AKST: 'Alaska', AKDT: 'Alaska', HST: 'Hawaii-Aleutian', HDT: 'Hawaii-Aleutian', AST: 'Atlantic', ADT: 'Atlantic' };
  const US_GENERIC = { Eastern: 'ET', Central: 'CT', Mountain: 'MT', Pacific: 'PT', Alaska: 'AKT', 'Hawaii-Aleutian': 'HST', Atlantic: 'AT' };
  const LONG_NAMES = {
    CET: 'Central European Standard Time', CEST: 'Central European Summer Time', EET: 'Eastern European Standard Time', EEST: 'Eastern European Summer Time',
    WET: 'Western European Standard Time', WEST: 'Western European Summer Time', BST: 'British Summer Time', JST: 'Japan Standard Time', KST: 'Korean Standard Time',
    HKT: 'Hong Kong Standard Time', AEST: 'Australian Eastern Standard Time', AEDT: 'Australian Eastern Daylight Time', ACST: 'Australian Central Standard Time',
    ACDT: 'Australian Central Daylight Time', AWST: 'Australian Western Standard Time', NZST: 'New Zealand Standard Time', NZDT: 'New Zealand Daylight Time',
    MSK: 'Moscow Standard Time', SAST: 'South Africa Standard Time', WAT: 'West Africa Standard Time', EAT: 'East Africa Time', CAT: 'Central Africa Time',
    PKT: 'Pakistan Standard Time', WIB: 'Western Indonesia Time', IDT: 'Israel Daylight Time',
  };
  function gmtOffset(off, long) {
    if (off === 0) return 'GMT';
    const a = Math.abs(off), h = Math.floor(a / 3600), m = Math.floor(a / 60) % 60, s = off < 0 ? '-' : '+';
    if (long) return 'GMT' + s + String(h).padStart(2, '0') + ':' + String(m).padStart(2, '0');
    return 'GMT' + s + h + (m ? ':' + String(m).padStart(2, '0') : '');
  }
  function tzDisplay(tz, f, style) {
    if (tz === 'UTC') return style === 'long' ? 'Coordinated Universal Time' : style === 'short' ? 'UTC' : 'GMT';
    const abbr = f.abbr, us = /^America\//.test(tz) || tz === 'Pacific/Honolulu' ? US_ZONES[abbr] : null;
    switch (style) {
      case 'short': return us && abbr !== 'AST' && abbr !== 'ADT' ? abbr : gmtOffset(f.off, false);
      case 'shortOffset': return gmtOffset(f.off, false);
      case 'longOffset': return gmtOffset(f.off, true);
      case 'shortGeneric': return us ? US_GENERIC[us] : gmtOffset(f.off, false);
      case 'longGeneric': return us ? us + ' Time' : gmtOffset(f.off, true);
      default: {
        if (us) return us + (f.dst ? ' Daylight' : ' Standard') + ' Time';
        if (abbr === 'GMT' && /^Europe\/(London|Dublin|Belfast|Guernsey|Isle_of_Man|Jersey)$|^Africa\//.test(tz)) return 'Greenwich Mean Time';
        if (abbr === 'IST') return tz === 'Asia/Calcutta' ? 'India Standard Time' : tz === 'Europe/Dublin' ? 'Irish Standard Time' : 'Israel Standard Time';
        if (abbr === 'CST' && /^Asia\/(Shanghai|Chongqing|Harbin|Macau)$/.test(tz)) return 'China Standard Time';
        if (abbr === 'CST' && tz === 'Asia/Taipei') return 'Taipei Standard Time';
        return LONG_NAMES[abbr] || gmtOffset(f.off, true);
      }
    }
  }

  // ------------------------------------------------------------ DateTimeFormat
  const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
  const pad2 = (n) => (n < 10 ? '0' : '') + n;
  const DATE_FIELDS = ['weekday', 'year', 'month', 'day'], TIME_FIELDS = ['dayPeriod', 'hour', 'minute', 'second', 'fractionalSecondDigits'];
  const COMPONENTS = [
    ['weekday', ['narrow', 'short', 'long']], ['era', ['narrow', 'short', 'long']], ['year', ['2-digit', 'numeric']], ['month', ['2-digit', 'numeric', 'narrow', 'short', 'long']],
    ['day', ['2-digit', 'numeric']], ['dayPeriod', ['narrow', 'short', 'long']], ['hour', ['2-digit', 'numeric']], ['minute', ['2-digit', 'numeric']], ['second', ['2-digit', 'numeric']],
    ['fractionalSecondDigits', null], ['timeZoneName', ['short', 'long', 'shortOffset', 'longOffset', 'shortGeneric', 'longGeneric']],
  ];
  const STYLES = {
    date: { full: { weekday: 'long', month: 'long', day: 'numeric', year: 'numeric' }, long: { month: 'long', day: 'numeric', year: 'numeric' }, medium: { month: 'short', day: 'numeric', year: 'numeric' }, short: { month: 'numeric', day: 'numeric', year: '2-digit' } },
    time: { full: { hour: 'numeric', minute: '2-digit', second: '2-digit', timeZoneName: 'long' }, long: { hour: 'numeric', minute: '2-digit', second: '2-digit', timeZoneName: 'short' }, medium: { hour: 'numeric', minute: '2-digit', second: '2-digit' }, short: { hour: 'numeric', minute: '2-digit' } },
  };
  // `required`/`defaults` follow ToDateTimeOptions: 'any'/'date' for the
  // constructor, 'date', 'time' or 'any'/'all' for Date#toLocale*String.
  function createDTF(self, locales, options, required, defaults) {
    const { locale, kw } = resolveLocale(locales);
    const o = toOptions(options);
    const opt = getOpt.bind('DateTimeFormat');
    opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    const calendar = opt(o, 'calendar', null, kw.ca);
    if (calendar !== undefined && !/^[a-z0-9]{3,8}(-[a-z0-9]{3,8})*$/i.test(calendar)) throw new RangeError('Invalid calendar : ' + calendar);
    const ns = opt(o, 'numberingSystem', null, kw.nu);
    if (ns !== undefined && !/^[a-z0-9]{3,8}(-[a-z0-9]{3,8})*$/i.test(ns)) throw new RangeError('Invalid numberingSystem : ' + ns);
    const hour12 = o.hour12 === undefined ? undefined : Boolean(o.hour12);
    let hourCycle = opt(o, 'hourCycle', ['h11', 'h12', 'h23', 'h24'], undefined);
    if (hour12 !== undefined) hourCycle = hour12 ? 'h12' : 'h23';
    else if (hourCycle === undefined && ['h11', 'h12', 'h23', 'h24'].includes(kw.hc)) hourCycle = kw.hc;
    let tz = o.timeZone;
    if (tz === undefined) tz = N.tzResolve();
    else {
      const c = N.tzResolve(String(tz));
      if (c === null) throw new RangeError('Invalid time zone specified: ' + String(tz));
      tz = c;
    }
    const tzKey = tz;
    tz = ICU_ZONE[tz] || tz;
    const f = {};
    for (const [k, vals] of COMPONENTS) {
      if (k === 'fractionalSecondDigits') f[k] = getNum(o, k, 1, 3, undefined);
      else f[k] = opt(o, k, vals, undefined);
    }
    opt(o, 'formatMatcher', ['basic', 'best fit'], 'best fit');
    const dateStyle = opt(o, 'dateStyle', ['full', 'long', 'medium', 'short'], undefined);
    const timeStyle = opt(o, 'timeStyle', ['full', 'long', 'medium', 'short'], undefined);
    const ext = [];
    if (kw.ca === 'gregory' && (calendar === undefined || calendar === 'gregory')) ext.push('ca-gregory');
    if (o.hour12 === undefined && o.hourCycle === undefined && ['h11', 'h12', 'h23', 'h24'].includes(kw.hc)) ext.push('hc-' + kw.hc);
    if (kw.nu === 'latn' && (ns === undefined || ns === 'latn')) ext.push('nu-latn');
    const r = { locale: ext.length ? locale + '-u-' + ext.join('-') : locale, calendar: 'gregory', numberingSystem: 'latn', timeZone: tz, tzKey };
    if (dateStyle || timeStyle) {
      for (const [k] of COMPONENTS) if (f[k] !== undefined) throw new TypeError(`Can't set option ${k} when ${dateStyle ? 'dateStyle' : 'timeStyle'} is used`);
      if (required === 'date' && !dateStyle) throw new TypeError('Invalid option : timeStyle');
      if (required === 'time' && !timeStyle) throw new TypeError('Invalid option : dateStyle');
      Object.assign(f, dateStyle ? (dateStyle === 'short' && shortDateStyle(locale)) || STYLES.date[dateStyle] : {}, timeStyle ? STYLES.time[timeStyle] : {});
      r.dateStyle = dateStyle; r.timeStyle = timeStyle;
    } else {
      const needDefaults = !((required === 'date' || required === 'any') && DATE_FIELDS.some((k) => f[k] !== undefined)) &&
        !((required === 'time' || required === 'any') && TIME_FIELDS.some((k) => f[k] !== undefined));
      if (needDefaults && (defaults === 'date' || defaults === 'all')) Object.assign(f, { year: 'numeric', month: 'numeric', day: 'numeric' });
      if (needDefaults && (defaults === 'time' || defaults === 'all')) Object.assign(f, { hour: 'numeric', minute: 'numeric', second: 'numeric' });
    }
    // Pattern normalization (en-US skeleton matching).
    if (f.hour) {
      if (f.minute) f.minute = '2-digit';
      r.hourCycle = hourCycle || (dateOrder(locale).h23 ? 'h23' : 'h12');
      r.hour12 = r.hourCycle === 'h11' || r.hourCycle === 'h12';
      if (!r.hour12) f.hour = '2-digit';
    }
    if (f.second && (f.minute || f.hour)) f.second = '2-digit';
    if (f.minute && f.second && !f.hour) f.minute = '2-digit';
    r.f = f;
    hidden(self, '_dtf', r);
  }
  class DateTimeFormat {
    constructor(locales, options) { createDTF(this, locales, options, 'any', 'date'); }
    get format() {
      const r = brand(this, '_dtf', 'DateTimeFormat', 'format');
      if (!r.bound) r.bound = (d) => partsToString(dateParts(r, toTime(d))).replace(/ /g, ' ');
      return r.bound;
    }
    formatToParts(d) { return dateParts(brand(this, '_dtf', 'DateTimeFormat', 'formatToParts'), toTime(d)); }
    formatRange(a, b) { return partsToString(this.formatRangeToParts(a, b)).replace(/ /g, ' '); }
    formatRangeToParts(a, b) {
      const r = brand(this, '_dtf', 'DateTimeFormat', 'formatRange');
      if (a === undefined || b === undefined) throw new TypeError('startDate or endDate is undefined');
      return rangeParts(r, toTime(a), toTime(b));
    }
    resolvedOptions() {
      const r = brand(this, '_dtf', 'DateTimeFormat', 'resolvedOptions');
      const out = { locale: r.locale, calendar: r.calendar, numberingSystem: r.numberingSystem, timeZone: r.timeZone };
      if (r.hourCycle) { out.hourCycle = r.hourCycle; out.hour12 = r.hour12; }
      if (r.dateStyle || r.timeStyle) {
        if (r.dateStyle) out.dateStyle = r.dateStyle;
        if (r.timeStyle) out.timeStyle = r.timeStyle;
        return out;
      }
      for (const [k] of COMPONENTS) if (r.f[k] !== undefined) out[k] = r.f[k];
      return out;
    }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(DateTimeFormat, 'Intl.DateTimeFormat');
  function toTime(d) {
    const t = d === undefined ? Date.now() : Number(d instanceof Date ? d.getTime() : d);
    if (!isFinite(t) || Math.abs(t) > 8.64e15) throw new RangeError('Invalid time value');
    return Math.trunc(t) + 0;
  }
  function fieldsAt(r, t) {
    const info = tzInfo(r.tzKey, t);
    const d = new Date(t + info[0] * 1000);
    return { y: d.getUTCFullYear(), m: d.getUTCMonth(), d: d.getUTCDate(), wd: d.getUTCDay(), h: d.getUTCHours(), mi: d.getUTCMinutes(), s: d.getUTCSeconds(), ms: d.getUTCMilliseconds(), off: info[0], abbr: info[1], dst: info[2] };
  }
  // Numeric date field order and separator per locale (a small table; other
  // locales use en-US's).
  function dateOrder(locale) {
    const l = String(locale || 'en-US').split('-u-')[0];
    const lang = l.split('-')[0].toLowerCase();
    const region = (l.split('-').find((x, i) => i > 0 && /^[A-Za-z]{2}$/.test(x)) || '').toUpperCase();
    // pad: zero-pad day/month in a full date; padMD: in a month/day date;
    // md: order and separator of a month/day date when it differs.
    if (l === 'en-CA' || l === 'fr-CA') return { order: 'ymd', sep: '-', pad: true, padMD: true, md: ['mdy', '-'], dp: l === 'en-CA' ? ['a.m.', 'p.m.'] : null };
    if (lang === 'sv' || lang === 'lt') return { order: 'ymd', sep: '-', pad: true, padMD: false, md: ['dmy', '/'], h23: true };
    if (lang === 'ja' || lang === 'zh' || l === 'en-ZA') return { order: 'ymd', sep: '/', pad: l === 'en-ZA', padMD: false, md: lang === 'ja' || lang === 'zh' ? ['mdy', '/'] : null, h23: lang !== 'en' };
    if (lang === 'en') {
      if (!region || ['US', 'PH', 'AS', 'GU', 'PR', 'UM', 'VI'].includes(region)) return { order: 'mdy', sep: '/', pad: false, padMD: false };
      const gb = region === 'GB' || region === 'IE';
      return { order: 'dmy', sep: '/', pad: true, padMD: gb, en: true, dp: ['am', 'pm'], h23: gb };
    }
    if (['de', 'ru', 'pl', 'tr', 'nb', 'no', 'fi', 'cs', 'da', 'uk', 'ro', 'sk', 'hr', 'sl', 'et', 'lv', 'is'].includes(lang)) {
      const p = !['de', 'fi', 'cs', 'sk'].includes(lang);
      return { order: 'dmy', sep: '.', trailingDot: true, pad: p, padMD: p, h23: true };
    }
    if (lang === 'nl') return { order: 'dmy', sep: '-', pad: false, padMD: false, h23: true };
    if (['fr', 'es', 'it', 'pt', 'el', 'ca', 'id', 'vi', 'ms', 'he'].includes(lang)) {
      const p = lang === 'fr' || lang === 'pt' || lang === 'ca';
      return { order: 'dmy', sep: '/', pad: p, padMD: p, h23: true };
    }
    return { order: 'mdy', sep: '/', pad: false, padMD: false };
  }
  // dateStyle: 'short' differs most between locales.
  function shortDateStyle(locale) {
    const lo = dateOrder(locale), l = String(locale || '').split('-u-')[0];
    if (lo.order === 'ymd') return { year: 'numeric', month: '2-digit', day: '2-digit' };
    if (l === 'en-GB' || l === 'en-IE' || ['fr', 'pt', 'ca', 'nl'].includes(l.split('-')[0])) return { day: '2-digit', month: '2-digit', year: 'numeric' };
    if (lo.sep === '.' || lo.sep === '-') return { day: '2-digit', month: '2-digit', year: '2-digit' };
    return null;
  }
  function dayPeriodName(h, mi, style) {
    if (h === 12 && mi === 0) return style === 'narrow' ? 'n' : 'noon';
    if (h >= 6 && h < 12) return 'in the morning';
    if (h >= 12 && h < 18) return 'in the afternoon';
    if (h >= 18 && h < 21) return 'in the evening';
    return 'at night';
  }
  function dateParts(r, t) {
    const o = r.f, F = fieldsAt(r, t);
    const parts = [];
    const lit = (v) => parts.push({ type: 'literal', value: v });
    const push = (type, value) => parts.push({ type, value });
    const yearVal = F.y <= 0 && o.era ? 1 - F.y : F.y;
    const yearStr = o.year === '2-digit' ? pad2(((yearVal % 100) + 100) % 100) : String(yearVal);
    const monthStr = o.month === 'long' ? MONTHS[F.m] : o.month === 'short' ? MONTHS[F.m].slice(0, 3) : o.month === 'narrow' ? MONTHS[F.m][0] : o.month === '2-digit' ? pad2(F.m + 1) : String(F.m + 1);
    const dayStr = o.day === '2-digit' ? pad2(F.d) : String(F.d);
    const wdStr = o.weekday === 'long' ? DAYS[F.wd] : o.weekday === 'short' ? DAYS[F.wd].slice(0, 3) : DAYS[F.wd][0];
    const eraStr = o.era === 'long' ? (F.y > 0 ? 'Anno Domini' : 'Before Christ') : o.era === 'narrow' ? (F.y > 0 ? 'A' : 'B') : (F.y > 0 ? 'AD' : 'BC');
    const hasDate = o.weekday || o.year || o.month || o.day;
    const hasTime = o.hour || o.minute || o.second || o.fractionalSecondDigits || o.dayPeriod;
    // Date.
    if (hasDate) {
      const textual = o.month && o.month !== 'numeric' && o.month !== '2-digit';
      const lo = dateOrder(r.locale);
      if (textual && lo.order === 'dmy' && lo.en) {
        // en-GB style: "Sunday 11 October 2026".
        if (o.weekday && o.day) { push('weekday', wdStr); lit(' '); }
        if (o.day) { push('day', dayStr); lit(' '); }
        push('month', monthStr);
        if (o.year) { lit(' '); push('year', yearStr); }
        if (o.weekday && !o.day) { lit(' '); push('weekday', wdStr); }
      } else if (textual) {
        if (o.weekday && o.day) { push('weekday', wdStr); lit(', '); }
        push('month', monthStr);
        if (o.day) { lit(' '); push('day', dayStr); }
        if (o.year) { lit(o.day ? ', ' : ' '); push('year', yearStr); }
        if (o.weekday && !o.day) { lit(' '); push('weekday', wdStr); }
      } else if (o.month || o.day) {
        if (o.weekday) { push('weekday', wdStr); lit(', '); }
        // Numeric dates follow the locale's field order and separator
        // (en-US 10/11/2026, en-GB 11/10/2026, en-CA 2026-10-11, de 11.10.2026).
        const md = !o.year && o.month && o.day && lo.md;
        const order = md ? lo.md[0] : lo.order, sep = md ? lo.md[1] : lo.sep;
        const pad = o.year ? lo.pad : lo.padMD;
        const padNum = (v) => (pad && /^\d$/.test(v) ? '0' + v : v);
        const f = { month: o.month ? ['month', padNum(monthStr)] : null, day: o.day ? ['day', padNum(dayStr)] : null, year: o.year ? ['year', yearStr] : null };
        const keys = order === 'ymd' ? ['year', 'month', 'day'] : order === 'dmy' ? ['day', 'month', 'year'] : ['month', 'day', 'year'];
        const seq = keys.map((k) => f[k]).filter(Boolean);
        seq.forEach(([k, v], i) => { if (i) lit(sep); push(k, v); });
        if (lo.sep === '.' && lo.trailingDot && !o.year) lit('.');
      } else {
        if (o.year) push('year', yearStr);
        if (o.weekday) { if (o.year) lit(' '); push('weekday', wdStr); }
      }
      if (o.era) { lit(' '); push('era', eraStr); }
    } else if (o.era) { push('era', eraStr); }
    // Time.
    if (hasTime) {
      if (hasDate) lit(o.month === 'long' ? ' at ' : o.weekday === 'short' && o.minute && !o.year && !o.month && !o.day ? ' ' : ', ');
      const h12 = r.hour12;
      if (o.hour) {
        let h = F.h;
        if (r.hourCycle === 'h11') h = h % 12;
        else if (r.hourCycle === 'h12') h = h % 12 || 12;
        else if (r.hourCycle === 'h24') h = h || 24;
        push('hour', o.hour === '2-digit' ? pad2(h) : String(h));
      }
      if (o.minute) { if (o.hour) lit(':'); push('minute', o.minute === '2-digit' ? pad2(F.mi) : String(F.mi)); }
      const oddSecond = o.second && o.hour && !o.minute;
      if (o.second && !oddSecond) { if (o.hour || o.minute) lit(':'); push('second', o.second === '2-digit' ? pad2(F.s) : String(F.s)); }
      if (o.fractionalSecondDigits && !oddSecond) { if (o.second) lit('.'); push('fractionalSecond', String(F.ms).padStart(3, '0').slice(0, o.fractionalSecondDigits)); }
      if (o.dayPeriod && (!o.hour || h12)) { if (parts.length && parts[parts.length - 1].type !== 'literal') lit(' '); push('dayPeriod', dayPeriodName(F.h, F.mi, o.dayPeriod)); }
      else if (o.hour && h12) { const dp = dateOrder(r.locale).dp || ['AM', 'PM']; lit(NNBSP); push('dayPeriod', F.h < 12 ? dp[0] : dp[1]); }
      // ICU appends fields its pattern can't place.
      if (oddSecond) { lit(' (second: '); push('second', String(F.s)); lit(')'); }
    }
    if (o.timeZoneName) { lit(hasTime ? ' ' : ', '); push('timeZoneName', tzDisplay(r.timeZone, F, o.timeZoneName)); }
    return parts;
  }
  // Interval formats: share the parts coarser than the greatest differing field.
  const RANK = { era: 7, year: 6, month: 5, day: 4, weekday: 4, dayPeriod: 3, hour: 2, minute: 1, second: 0, fractionalSecond: -1, timeZoneName: 8 };
  function rangeParts(r, a, b) {
    const A = fieldsAt(r, a), B = fieldsAt(r, b), o = r.f;
    const diffs = [];
    if (o.era && (A.y > 0) !== (B.y > 0)) diffs.push(7);
    if ((o.year || o.era) && A.y !== B.y) diffs.push(6);
    if (o.month && (A.y !== B.y || A.m !== B.m)) diffs.push(5);
    if ((o.day || o.weekday) && (A.y !== B.y || A.m !== B.m || A.d !== B.d)) diffs.push(4);
    const sameDay = A.y === B.y && A.m === B.m && A.d === B.d;
    if (r.hour12 && (o.hour || o.dayPeriod) && (!sameDay || (A.h < 12) !== (B.h < 12))) diffs.push(3);
    if (o.hour && (!sameDay || A.h !== B.h)) diffs.push(2);
    if (o.minute && (!sameDay || A.h !== B.h || A.mi !== B.mi)) diffs.push(1);
    if (o.second && a - a % 1000 !== b - b % 1000) diffs.push(0);
    const pa = dateParts(r, a), pb = dateParts(r, b);
    if (!diffs.length) return pa.map((p) => Object.assign(p, { source: 'shared' }));
    const G = Math.max(...diffs);
    const hasTime = o.hour || o.minute || o.second;
    const numericDate = o.month === 'numeric' || o.month === '2-digit' || (!o.month && o.day);
    const collapse = !(numericDate && G >= 4) && !(hasTime && G >= 4);
    const same = (x, y) => x.type === y.type && x.value === y.value && (x.type === 'literal' || RANK[x.type] > G);
    let pre = 0, suf = 0;
    if (collapse) {
      while (pre < pa.length && pre < pb.length && same(pa[pre], pb[pre])) pre++;
      while (suf < pa.length - pre && suf < pb.length - pre && same(pa[pa.length - 1 - suf], pb[pb.length - 1 - suf])) suf++;
    }
    const tag = (arr, s) => arr.map((p) => Object.assign({}, p, { source: s }));
    return tag(pa.slice(0, pre), 'shared').concat(tag(pa.slice(pre, pa.length - suf), 'startRange'), [{ type: 'literal', value: THIN + '–' + THIN, source: 'shared' }], tag(pb.slice(pre, pb.length - suf), 'endRange'), tag(pa.slice(pa.length - suf), 'shared'));
  }

  // ------------------------------------------------------------ Collator
  class Collator {
    constructor(locales, options) {
      const { locale, kw } = resolveLocale(locales);
      const o = toOptions(options);
      const opt = getOpt.bind('Collator');
      const usage = opt(o, 'usage', ['sort', 'search'], 'sort');
      opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      opt(o, 'collation', null, kw.co);
      const numeric = o.numeric === undefined ? kw.kn === '' || kw.kn === 'true' : Boolean(o.numeric);
      const caseFirst = opt(o, 'caseFirst', ['upper', 'lower', 'false'], ['upper', 'lower', 'false'].includes(kw.kf) ? kw.kf : 'false');
      const sensitivity = opt(o, 'sensitivity', ['base', 'accent', 'case', 'variant'], 'variant');
      const ignorePunctuation = getBool(o, 'ignorePunctuation', false);
      const ext = [];
      if (o.caseFirst === undefined && ['upper', 'lower', 'false'].includes(kw.kf)) ext.push('kf-' + kw.kf);
      if (kw.kn !== undefined && o.numeric === undefined) ext.push(kw.kn === '' || kw.kn === 'true' ? 'kn' : 'kn-' + kw.kn);
      const r = { locale: ext.length ? locale + '-u-' + ext.join('-') : locale, usage, sensitivity, ignorePunctuation, collation: 'default', numeric, caseFirst };
      r.flags = ['base', 'accent', 'case', 'variant'].indexOf(sensitivity) | (numeric ? 4 : 0) | (ignorePunctuation ? 8 : 0) | (caseFirst === 'upper' ? 16 : 0);
      hidden(this, '_co', r);
    }
    get compare() {
      const r = brand(this, '_co', 'Collator', 'compare');
      if (!r.bound) r.bound = (a, b) => N.collate(String(a), String(b), r.flags);
      return r.bound;
    }
    resolvedOptions() {
      const r = brand(this, '_co', 'Collator', 'resolvedOptions');
      return { locale: r.locale, usage: r.usage, sensitivity: r.sensitivity, ignorePunctuation: r.ignorePunctuation, collation: r.collation, numeric: r.numeric, caseFirst: r.caseFirst };
    }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(Collator, 'Intl.Collator');

  // ------------------------------------------------------------ ListFormat
  class ListFormat {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.ListFormat requires 'new'");
      const { locale } = resolveLocale(locales);
      const o = toOptions(options);
      const opt = getOpt.bind('ListFormat');
      opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      hidden(this, '_lf', { locale, type: opt(o, 'type', ['conjunction', 'disjunction', 'unit'], 'conjunction'), style: opt(o, 'style', ['long', 'short', 'narrow'], 'long') });
    }
    format(list) { return this.formatToParts(list).map((p) => p.value).join(''); }
    formatToParts(list) {
      const r = brand(this, '_lf', 'ListFormat', 'format');
      const items = [];
      for (const x of (list === undefined ? [] : list)) { if (typeof x !== 'string') throw new TypeError('Iterable yielded ' + x + ' which is not a string'); items.push(x); }
      let mid = ', ', last2, lastN;
      if (r.type === 'disjunction') { last2 = ' or '; lastN = ', or '; }
      else if (r.type === 'unit') { if (r.style === 'narrow') { mid = ' '; last2 = lastN = ' '; } else { last2 = lastN = ', '; } }
      else if (r.style === 'long') { last2 = ' and '; lastN = ', and '; }
      else if (r.style === 'short') { last2 = ' & '; lastN = ', & '; }
      else { last2 = lastN = ', '; }
      const parts = [];
      items.forEach((x, i) => {
        if (i) parts.push({ type: 'literal', value: i === items.length - 1 ? (items.length === 2 ? last2 : lastN) : mid });
        parts.push({ type: 'element', value: x });
      });
      return parts;
    }
    resolvedOptions() { const r = brand(this, '_lf', 'ListFormat', 'resolvedOptions'); return { locale: r.locale, type: r.type, style: r.style }; }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(ListFormat, 'Intl.ListFormat');

  // ------------------------------------------------------------ Segmenter
  class Segmenter {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.Segmenter requires 'new'");
      const { locale } = resolveLocale(locales);
      const o = toOptions(options);
      getOpt.call('Segmenter', o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      hidden(this, '_sg', { locale, granularity: getOpt.call('Segmenter', o, 'granularity', ['grapheme', 'word', 'sentence'], 'grapheme') });
    }
    segment(str) { return new Segments(brand(this, '_sg', 'Segmenter', 'segment'), String(str)); }
    resolvedOptions() { const r = brand(this, '_sg', 'Segmenter', 'resolvedOptions'); return { locale: r.locale, granularity: r.granularity }; }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(Segmenter, 'Intl.Segmenter');
  class Segments {
    constructor(r, s) { hidden(this, '_r', r); hidden(this, '_s', s); hidden(this, '_b', null); }
    _bounds() {
      if (!this._b) {
        const flat = N.segment(this._s, this._r.granularity), starts = [], words = [];
        for (let i = 0; i < flat.length; i += 2) { starts.push(flat[i]); words.push(flat[i + 1]); }
        this._b = { starts, words };
      }
      return this._b;
    }
    _data(i) {
      const { starts, words } = this._bounds();
      const start = starts[i], end = i + 1 < starts.length ? starts[i + 1] : this._s.length;
      const d = { segment: this._s.slice(start, end), index: start, input: this._s };
      if (this._r.granularity === 'word') d.isWordLike = words[i];
      return d;
    }
    containing(index) {
      let n = index === undefined ? 0 : Math.trunc(Number(index)) || 0;
      if (n < 0 || n >= this._s.length) return undefined;
      const { starts } = this._bounds();
      let lo = 0, hi = starts.length - 1;
      while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (starts[mid] <= n) lo = mid; else hi = mid - 1; }
      return this._data(lo);
    }
    [Symbol.iterator]() {
      const self = this; let i = 0;
      const it = { next() { const { starts } = self._bounds(); if (i >= starts.length) return { done: true, value: undefined }; return { done: false, value: self._data(i++) }; }, [Symbol.iterator]() { return this; } };
      Object.defineProperty(it, Symbol.toStringTag, { value: 'Segmenter String Iterator' });
      return it;
    }
  }

  // ------------------------------------------------------------ DisplayNames
  const REGIONS = {
    US: 'United States', GB: 'United Kingdom', CA: 'Canada', AU: 'Australia', NZ: 'New Zealand', IE: 'Ireland', DE: 'Germany', FR: 'France', ES: 'Spain', IT: 'Italy', PT: 'Portugal',
    NL: 'Netherlands', BE: 'Belgium', CH: 'Switzerland', AT: 'Austria', SE: 'Sweden', NO: 'Norway', DK: 'Denmark', FI: 'Finland', IS: 'Iceland', PL: 'Poland', CZ: 'Czechia',
    SK: 'Slovakia', HU: 'Hungary', RO: 'Romania', BG: 'Bulgaria', GR: 'Greece', TR: 'Türkiye', RU: 'Russia', UA: 'Ukraine', BY: 'Belarus', IL: 'Israel', EG: 'Egypt',
    SA: 'Saudi Arabia', AE: 'United Arab Emirates', IR: 'Iran', IQ: 'Iraq', IN: 'India', PK: 'Pakistan', BD: 'Bangladesh', CN: 'China', HK: 'Hong Kong SAR China', TW: 'Taiwan',
    JP: 'Japan', KR: 'South Korea', KP: 'North Korea', SG: 'Singapore', MY: 'Malaysia', ID: 'Indonesia', TH: 'Thailand', VN: 'Vietnam', PH: 'Philippines', MX: 'Mexico',
    BR: 'Brazil', AR: 'Argentina', CL: 'Chile', CO: 'Colombia', PE: 'Peru', VE: 'Venezuela', ZA: 'South Africa', NG: 'Nigeria', KE: 'Kenya', ET: 'Ethiopia', MA: 'Morocco',
    DZ: 'Algeria', TN: 'Tunisia', GH: 'Ghana', ZZ: 'Unknown Region', '001': 'world', 150: 'Europe', 419: 'Latin America', EU: 'European Union', UN: 'United Nations',
  };
  const LANGUAGES = {
    en: 'English', fr: 'French', de: 'German', es: 'Spanish', it: 'Italian', pt: 'Portuguese', nl: 'Dutch', sv: 'Swedish', no: 'Norwegian', nb: 'Norwegian Bokmål', da: 'Danish',
    fi: 'Finnish', is: 'Icelandic', pl: 'Polish', cs: 'Czech', sk: 'Slovak', hu: 'Hungarian', ro: 'Romanian', bg: 'Bulgarian', el: 'Greek', tr: 'Turkish', ru: 'Russian',
    uk: 'Ukrainian', be: 'Belarusian', he: 'Hebrew', ar: 'Arabic', fa: 'Persian', ur: 'Urdu', hi: 'Hindi', bn: 'Bangla', ta: 'Tamil', te: 'Telugu', mr: 'Marathi',
    zh: 'Chinese', ja: 'Japanese', ko: 'Korean', th: 'Thai', vi: 'Vietnamese', id: 'Indonesian', ms: 'Malay', fil: 'Filipino', sw: 'Swahili', af: 'Afrikaans', ca: 'Catalan',
    eu: 'Basque', gl: 'Galician', ga: 'Irish', cy: 'Welsh', hr: 'Croatian', sr: 'Serbian', sl: 'Slovenian', lt: 'Lithuanian', lv: 'Latvian', et: 'Estonian', la: 'Latin', eo: 'Esperanto',
    'en-US': 'American English', 'en-GB': 'British English', 'en-AU': 'Australian English', 'en-CA': 'Canadian English', 'fr-CA': 'Canadian French', 'es-MX': 'Mexican Spanish',
    'es-419': 'Latin American Spanish', 'pt-BR': 'Brazilian Portuguese', 'de-AT': 'Austrian German', 'de-CH': 'Swiss High German', 'fr-CH': 'Swiss French', 'nl-BE': 'Flemish', 'es-ES': 'European Spanish', 'pt-PT': 'European Portuguese', 'zh-Hans': 'Simplified Chinese', 'zh-Hant': 'Traditional Chinese',
  };
  const SCRIPTS = { Latn: 'Latin', Cyrl: 'Cyrillic', Arab: 'Arabic', Hans: 'Simplified Han', Hant: 'Traditional Han', Grek: 'Greek', Hebr: 'Hebrew', Deva: 'Devanagari', Jpan: 'Japanese', Kore: 'Korean', Thai: 'Thai' };
  const DT_FIELDS = { era: 'era', year: 'year', quarter: 'quarter', month: 'month', weekOfYear: 'week', weekday: 'day of the week', day: 'day', dayPeriod: 'AM/PM', hour: 'hour', minute: 'minute', second: 'second', timeZoneName: 'time zone' };
  class DisplayNames {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.DisplayNames requires 'new'");
      const { locale } = resolveLocale(locales);
      if (options === undefined) throw new TypeError('Cannot convert undefined or null to object');
      const o = toOptions(options);
      const opt = getOpt.bind('DisplayNames');
      opt(o, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
      const style = opt(o, 'style', ['narrow', 'short', 'long'], 'long');
      const type = opt(o, 'type', ['language', 'region', 'script', 'currency', 'calendar', 'dateTimeField'], undefined);
      if (type === undefined) throw new TypeError('Required option type is missing');
      const fallback = opt(o, 'fallback', ['code', 'none'], 'code');
      const languageDisplay = opt(o, 'languageDisplay', ['dialect', 'standard'], 'dialect');
      hidden(this, '_dn', { locale, style, type, fallback, languageDisplay });
    }
    of(code) {
      const r = brand(this, '_dn', 'DisplayNames', 'of');
      code = String(code);
      let name, canon = code;
      switch (r.type) {
        case 'region':
          if (!/^([A-Za-z]{2}|\d{3})$/.test(code)) throw new RangeError('invalid_argument');
          name = REGIONS[code]; break;
        case 'script':
          if (!/^[A-Za-z]{4}$/.test(code)) throw new RangeError('invalid_argument');
          canon = code[0].toUpperCase() + code.slice(1).toLowerCase(); name = SCRIPTS[canon]; break;
        case 'currency': {
          if (!/^[A-Za-z]{3}$/.test(code)) throw new RangeError('invalid_argument');
          canon = code.toUpperCase(); const c = CURRENCIES[canon]; name = c && c[2]; break;
        }
        case 'calendar': name = { gregory: 'Gregorian Calendar', buddhist: 'Buddhist Calendar', chinese: 'Chinese Calendar', hebrew: 'Hebrew Calendar', islamic: 'Hijri Calendar', japanese: 'Japanese Calendar', iso8601: 'ISO-8601 Calendar' }[code]; break;
        case 'dateTimeField':
          if (!(code in DT_FIELDS)) throw new RangeError('invalid_argument');
          name = DT_FIELDS[code]; break;
        default: {
          canon = canonTag(code);
          if (r.languageDisplay === 'dialect' && LANGUAGES[canon]) { name = LANGUAGES[canon]; break; }
          const p = canon.split('-'); const lang = LANGUAGES[p[0]];
          if (!lang) break;
          const extra = p.slice(1).map((x) => (x.length === 4 ? SCRIPTS[x] : REGIONS[x]) || x);
          name = extra.length ? lang + ' (' + extra.join(', ') + ')' : lang;
        }
      }
      if (name === undefined) return r.fallback === 'code' ? (r.type === 'region' ? code : canon) : undefined;
      return name;
    }
    resolvedOptions() { const r = brand(this, '_dn', 'DisplayNames', 'resolvedOptions'); const out = { locale: r.locale, style: r.style, type: r.type, fallback: r.fallback }; if (r.type === 'language') out.languageDisplay = r.languageDisplay; return out; }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(DisplayNames, 'Intl.DisplayNames');

  // ------------------------------------------------------------ Locale
  const LIKELY = {
    en: 'Latn-US', fr: 'Latn-FR', de: 'Latn-DE', es: 'Latn-ES', it: 'Latn-IT', pt: 'Latn-BR', nl: 'Latn-NL', sv: 'Latn-SE', da: 'Latn-DK', nb: 'Latn-NO', no: 'Latn-NO', fi: 'Latn-FI',
    pl: 'Latn-PL', cs: 'Latn-CZ', sk: 'Latn-SK', hu: 'Latn-HU', ro: 'Latn-RO', tr: 'Latn-TR', ru: 'Cyrl-RU', uk: 'Cyrl-UA', bg: 'Cyrl-BG', el: 'Grek-GR', he: 'Hebr-IL',
    ar: 'Arab-EG', fa: 'Arab-IR', ur: 'Arab-PK', hi: 'Deva-IN', bn: 'Beng-BD', th: 'Thai-TH', vi: 'Latn-VN', id: 'Latn-ID', ms: 'Latn-MY', ja: 'Jpan-JP', ko: 'Kore-KR',
    zh: 'Hans-CN', 'zh-TW': 'Hant-TW', 'zh-HK': 'Hant-HK', 'zh-MO': 'Hant-MO', 'zh-Hant': 'Hant-TW', sr: 'Cyrl-RS', ca: 'Latn-ES', sw: 'Latn-TZ', und: 'Latn-US',
  };
  class Locale {
    constructor(tag, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.Locale requires 'new'");
      if (typeof tag !== 'string' && !(tag instanceof Locale) && (typeof tag !== 'object' || tag === null)) throw new TypeError('First argument to Intl.Locale constructor can\'t be empty or missing');
      const c = canonTag(tag instanceof Locale ? tag.toString() : String(tag));
      const { base, kw } = splitTag(c);
      const o = toOptions(options);
      const p = base.split('-');
      const r = { language: p[0], script: undefined, region: undefined, variants: [] };
      for (const x of p.slice(1)) { if (x.length === 4 && /^[A-Z]/.test(x)) r.script = x; else if (/^([A-Z]{2}|\d{3})$/.test(x)) r.region = x; else r.variants.push(x); }
      const get = (k, re) => { const v = o[k]; if (v === undefined) return undefined; const s = String(v); if (re && !re.test(s)) throw new RangeError('Incorrect locale information provided'); return s; };
      const lang = get('language', /^[a-zA-Z]{2,3}$|^[a-zA-Z]{5,8}$/); if (lang) r.language = lang.toLowerCase();
      const script = get('script', /^[a-zA-Z]{4}$/); if (script) r.script = script[0].toUpperCase() + script.slice(1).toLowerCase();
      const region = get('region', /^([a-zA-Z]{2}|\d{3})$/); if (region) r.region = region.toUpperCase();
      r.calendar = get('calendar') || kw.ca; r.collation = get('collation') || kw.co; r.hourCycle = get('hourCycle') || kw.hc;
      if (r.hourCycle && !['h11', 'h12', 'h23', 'h24'].includes(r.hourCycle)) throw new RangeError('Incorrect locale information provided');
      r.caseFirst = get('caseFirst') || kw.kf; r.numberingSystem = get('numberingSystem') || kw.nu;
      const numeric = o.numeric !== undefined ? String(Boolean(o.numeric)) : kw.kn !== undefined ? (kw.kn === '' ? 'true' : kw.kn) : undefined;
      r.numeric = numeric;
      hidden(this, '_lc', r);
    }
    get baseName() { const r = brand(this, '_lc', 'Locale', 'baseName'); return [r.language, r.script, r.region].concat(r.variants).filter(Boolean).join('-'); }
    get language() { return brand(this, '_lc', 'Locale', 'language').language; }
    get script() { return brand(this, '_lc', 'Locale', 'script').script; }
    get region() { return brand(this, '_lc', 'Locale', 'region').region; }
    get calendar() { return brand(this, '_lc', 'Locale', 'calendar').calendar; }
    get collation() { return brand(this, '_lc', 'Locale', 'collation').collation; }
    get hourCycle() { return brand(this, '_lc', 'Locale', 'hourCycle').hourCycle; }
    get caseFirst() { return brand(this, '_lc', 'Locale', 'caseFirst').caseFirst; }
    get numberingSystem() { return brand(this, '_lc', 'Locale', 'numberingSystem').numberingSystem; }
    get numeric() { return brand(this, '_lc', 'Locale', 'numeric').numeric === 'true'; }
    toString() {
      const r = brand(this, '_lc', 'Locale', 'toString');
      const kw = [['ca', r.calendar], ['co', r.collation], ['hc', r.hourCycle], ['kf', r.caseFirst], ['kn', r.numeric === 'true' ? '' : r.numeric], ['nu', r.numberingSystem]].filter(([, v]) => v !== undefined);
      return this.baseName + (kw.length ? '-u-' + kw.map(([k, v]) => (v ? k + '-' + v : k)).join('-') : '');
    }
    toJSON() { return this.toString(); }
    maximize() {
      const r = brand(this, '_lc', 'Locale', 'maximize');
      const likely = (LIKELY[r.language + (r.region ? '-' + r.region : '')] || LIKELY[r.language + (r.script ? '-' + r.script : '')] || LIKELY[r.language] || '').split('-');
      const tag = [r.language === 'und' ? 'en' : r.language, r.script || likely[0], r.region || likely[1]].concat(r.variants).filter(Boolean).join('-');
      return new Locale(tag + this.toString().slice(this.baseName.length));
    }
    minimize() {
      const r = brand(this, '_lc', 'Locale', 'minimize');
      const likely = (LIKELY[r.language] || '').split('-');
      const tag = [r.language, r.script === likely[0] ? undefined : r.script, r.region === likely[1] ? undefined : r.region].concat(r.variants).filter(Boolean).join('-');
      return new Locale(tag + this.toString().slice(this.baseName.length));
    }
    getCalendars() { return [this.calendar || 'gregory']; }
    getCollations() { return [this.collation || 'emoji', 'eor'].slice(0, this.collation ? 1 : 2); }
    getHourCycles() { return [this.hourCycle || (['en', 'hi', 'ar', 'ko'].includes(this.language) ? 'h12' : 'h23')]; }
    getNumberingSystems() { return [this.numberingSystem || 'latn']; }
    getTimeZones() { return this.region === 'US' ? ['America/Chicago', 'America/Denver', 'America/Los_Angeles', 'America/New_York', 'Pacific/Honolulu'] : undefined; }
    getTextInfo() { return { direction: ['ar', 'he', 'fa', 'ur', 'yi', 'ps', 'sd', 'ug', 'dv'].includes(this.language) ? 'rtl' : 'ltr' }; }
    getWeekInfo() { const sun = ['US', 'CA', 'JP', 'IL', 'BR', 'MX', 'IN', 'KR', 'TW', 'HK', 'PH', 'SA', 'ZA'].includes(this.maximize().region); return { firstDay: sun ? 7 : 1, weekend: [6, 7], minimalDays: sun ? 1 : 4 }; }
  }
  setTag(Locale, 'Intl.Locale');
  for (const k of ['calendars', 'collations', 'hourCycles', 'numberingSystems', 'timeZones', 'textInfo', 'weekInfo']) {
    const m = 'get' + k[0].toUpperCase() + k.slice(1);
    Object.defineProperty(Locale.prototype, k, { get() { return this[m](); }, configurable: true });
  }

  // ------------------------------------------------------------ DurationFormat
  const DUR_UNITS = ['years', 'months', 'weeks', 'days', 'hours', 'minutes', 'seconds', 'milliseconds', 'microseconds', 'nanoseconds'];
  class DurationFormat {
    constructor(locales, options) {
      if (new.target === undefined) throw new TypeError("Constructor Intl.DurationFormat requires 'new'");
      const { locale } = resolveLocale(locales);
      const o = toOptions(options);
      hidden(this, '_du', { locale, numberingSystem: 'latn', style: getOpt.call('DurationFormat', o, 'style', ['long', 'short', 'narrow', 'digital'], 'short'), fractionalDigits: getNum(o, 'fractionalDigits', 0, 9, undefined) });
    }
    format(d) { return this.formatToParts(d).map((p) => p.value).join(''); }
    formatToParts(d) {
      const r = brand(this, '_du', 'DurationFormat', 'format');
      if (d === null || typeof d !== 'object') throw new TypeError('Invalid duration');
      const v = {};
      for (const u of DUR_UNITS) { const x = d[u] === undefined ? 0 : Number(d[u]); if (!Number.isInteger(x)) throw new RangeError('Invalid duration'); v[u] = x; }
      if (r.style === 'digital') {
        const parts = [];
        const lead = DUR_UNITS.slice(0, 4).filter((u) => v[u]);
        const lf = new ListFormat('en', { type: 'unit', style: 'short' });
        const head = lead.map((u) => new NumberFormat('en', { style: 'unit', unit: u.slice(0, -1), unitDisplay: 'short' }).format(v[u]));
        if (head.length) parts.push({ type: 'literal', value: lf.format(head) + ', ' });
        parts.push({ type: 'hour', value: String(v.hours) }, { type: 'literal', value: ':' }, { type: 'minute', value: pad2(v.minutes) }, { type: 'literal', value: ':' }, { type: 'second', value: pad2(v.seconds) });
        return parts;
      }
      const items = [];
      for (const u of DUR_UNITS) {
        if (!v[u]) continue;
        const unit = u.slice(0, -1);
        items.push(new NumberFormat('en', { style: 'unit', unit, unitDisplay: r.style === 'long' ? 'long' : r.style === 'narrow' ? 'narrow' : 'short' }).format(v[u]));
      }
      const lf = new ListFormat('en', { type: 'unit', style: r.style === 'long' ? 'long' : r.style === 'narrow' ? 'narrow' : 'short' });
      return lf.formatToParts(items).map((p) => (p.type === 'element' ? { type: 'literal', value: p.value } : p));
    }
    resolvedOptions() { const r = brand(this, '_du', 'DurationFormat', 'resolvedOptions'); return { locale: r.locale, numberingSystem: r.numberingSystem, style: r.style }; }
    static supportedLocalesOf(l, o) { return supportedLocalesOf(l, o); }
  }
  setTag(DurationFormat, 'Intl.DurationFormat');

  // ------------------------------------------------------------ Intl
  const UNIT_LIST = Object.keys(UNITS).sort();
  const Intl = {};
  const define = (k, v) => Object.defineProperty(Intl, k, { value: v, writable: true, configurable: true });
  define('Collator', callable(Collator));
  define('DateTimeFormat', callable(DateTimeFormat));
  define('DisplayNames', DisplayNames);
  define('DurationFormat', DurationFormat);
  define('ListFormat', ListFormat);
  define('Locale', Locale);
  define('NumberFormat', callable(NumberFormat));
  define('PluralRules', PluralRules);
  define('RelativeTimeFormat', RelativeTimeFormat);
  define('Segmenter', Segmenter);
  define('getCanonicalLocales', function getCanonicalLocales(l) { return canonList(l); });
  define('supportedValuesOf', function supportedValuesOf(key) {
    switch (String(key)) {
      case 'calendar': return ['gregory', 'iso8601'];
      case 'collation': return ['emoji', 'eor'];
      case 'currency': return Object.keys(CURRENCIES).sort();
      case 'numberingSystem': return ['latn'];
      case 'timeZone': return N.tzList();
      case 'unit': return UNIT_LIST.slice();
      default: throw new RangeError('Invalid key : ' + String(key));
    }
  });
  Object.defineProperty(Intl, Symbol.toStringTag, { value: 'Intl', configurable: true });
  Object.defineProperty(global, 'Intl', { value: Intl, writable: true, configurable: true });

  // ------------------------------------------------------------ locale-sensitive built-ins
  const cache = new Map();
  function cached(key, make) {
    let v = cache.get(key);
    if (!v) { v = make(); cache.set(key, v); }
    return v;
  }
  const replace = (proto, name, fn) => Object.defineProperty(proto, name, { value: fn, writable: true, configurable: true });
  const dateTime = Date.prototype.getTime;
  function dateMethod(name, required, defaults) {
    replace(Date.prototype, name, {
      [name](locales, options) {
        const t = dateTime.call(this);
        if (isNaN(t)) return 'Invalid Date';
        const f = locales === undefined && options === undefined
          ? cached(name, () => { const x = Object.create(DateTimeFormat.prototype); createDTF(x, undefined, undefined, required, defaults); return x; })
          : (() => { const x = Object.create(DateTimeFormat.prototype); createDTF(x, locales, options, required, defaults); return x; })();
        return f.format(t);
      },
    }[name]);
  }
  dateMethod('toLocaleString', 'any', 'all');
  dateMethod('toLocaleDateString', 'date', 'date');
  dateMethod('toLocaleTimeString', 'time', 'time');
  const numValue = Number.prototype.valueOf;
  replace(Number.prototype, 'toLocaleString', function toLocaleString(locales, options) {
    const n = numValue.call(this);
    const f = locales === undefined && options === undefined ? cached('number', () => new NumberFormat()) : new NumberFormat(locales, options);
    return f.format(n);
  });
  if (typeof BigInt !== 'undefined') {
    const bigValue = BigInt.prototype.valueOf;
    replace(BigInt.prototype, 'toLocaleString', function toLocaleString(locales, options) {
      const n = bigValue.call(this);
      const f = locales === undefined && options === undefined ? cached('number', () => new NumberFormat()) : new NumberFormat(locales, options);
      return f.format(n);
    });
  }
  replace(Array.prototype, 'toLocaleString', function toLocaleString(locales, options) {
    const a = Object(this), len = Math.min(Number(a.length) || 0, 2 ** 32 - 1), out = [];
    for (let i = 0; i < len; i++) { const x = a[i]; out.push(x === undefined || x === null ? '' : String(Object(x).toLocaleString(locales, options))); }
    return out.join(',');
  });
  replace(String.prototype, 'localeCompare', function localeCompare(that, locales, options) {
    if (this === undefined || this === null) throw new TypeError('String.prototype.localeCompare called on null or undefined');
    const c = locales === undefined && options === undefined ? cached('collator', () => new Collator()) : new Collator(locales, options);
    return c.compare(String(this), String(that));
  });
})(globalThis);
