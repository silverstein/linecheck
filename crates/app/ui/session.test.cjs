// Regression tests for the zero-build UI. Only Node's built-in test runner is
// needed; Tauri events and the small DOM surface used here are deterministic.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
const source = html.match(/<script>([\s\S]*?)<\/script>/)[1];

function harness(overrides = {}) {
  let document;
  function element(tagName = 'div') {
    const classes = new Set();
    const el = {
      tagName: tagName.toUpperCase(), children: [], attributes: {}, dataset: {},
      style: { setProperty() {} }, handlers: {}, isConnected: true, disabled: false,
      clientHeight: 600, scrollTop: 0, _text: '',
      classList: {
        add(...names) { names.forEach(n => classes.add(n)); },
        remove(...names) { names.forEach(n => classes.delete(n)); },
        contains(name) { return classes.has(name); },
        toggle(name, force = !classes.has(name)) { if (force) classes.add(name); else classes.delete(name); return force; }
      },
      setAttribute(name, value) { this.attributes[name] = String(value); },
      removeAttribute(name) { delete this.attributes[name]; },
      addEventListener(name, fn) { this.handlers[name] = fn; },
      appendChild(child) { child.parentElement = this; this.children.push(child); return child; },
      append(...children) { children.forEach(child => this.appendChild(child)); },
      removeChild(child) { this.children.splice(this.children.indexOf(child), 1); },
      replaceChildren(...children) { this.children = []; this._text = ''; this.append(...children); },
      remove() { this.parentElement?.removeChild(this); this.isConnected = false; },
      insertBefore(child, before) { child.parentElement = this; this.children.splice(this.children.indexOf(before), 0, child); },
      querySelectorAll(selector) {
        const descendants = this.children.flatMap(child => [child, ...child.querySelectorAll('*')]);
        return descendants.filter(child => selector.split(',').some(part => {
          if (part === '*') return true;
          const classMatch = part.match(/^\.([\w-]+)/);
          if (classMatch) return child.classList.contains(classMatch[1]);
          if (part.startsWith('button')) return child.tagName === 'BUTTON' && !child.disabled;
          if (part.startsWith('input')) return child.tagName === 'INPUT' && !child.disabled;
          return false;
        }));
      },
      querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
      focus() { document.activeElement = this; },
      closest(selector) { return selector.split(',').some(s => s.toUpperCase() === this.tagName) ? this : null; },
      getBoundingClientRect() { return { top: 0, left: 40, height: 36 }; },
      getClientRects() { return [this.getBoundingClientRect()]; },
      scrollIntoView() {},
    };
    Object.defineProperties(el, {
      className: { get: () => [...classes].join(' '), set(value) { classes.clear(); String(value).split(/\s+/).filter(Boolean).forEach(n => classes.add(n)); } },
      textContent: { get() { return this._text + this.children.map(c => c.textContent).join(''); }, set(value) { this._text = String(value); this.children = []; } },
      firstChild: { get() { return this.children[0] || null; } }
    });
    return el;
  }
  const ids = new Map();
  const body = element('body');
  document = {
    body, documentElement: element('html'), activeElement: body, handlers: {},
    getElementById(id) {
      if (!ids.has(id)) { const el = element(id.startsWith('btn') ? 'button' : 'div'); el.id = id; ids.set(id, el); body.appendChild(el); }
      return ids.get(id);
    },
    createElement: element,
    addEventListener(name, fn) { this.handlers[name] = fn; },
    querySelector: selector => body.querySelector(selector),
    querySelectorAll: selector => body.querySelectorAll(selector),
  };
  const calls = [], listeners = new Map(), timers = new Map();
  let nextTimer = 1;
  const api = {
    async invoke(command, args) {
      calls.push([command, args]);
      if (overrides[command]) return overrides[command](args, api);
      // Keep boot-time settings hydration outside these session tests.
      if (command === 'load_settings') return new Promise(() => {});
    },
    async listen(name, fn) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(fn);
      return () => listeners.get(name).delete(fn);
    },
    emit(name, payload) { for (const fn of [...(listeners.get(name) || [])]) fn({ payload }); },
  };
  const context = vm.createContext({
    document, console, navigator: {},
    window: { __TAURI__: { core: { invoke: api.invoke }, event: { listen: api.listen }, dialog: { open: async () => null } }, addEventListener() {}, matchMedia: () => ({ matches: false }) },
    setTimeout(fn) { const id = nextTimer++; timers.set(id, fn); return id; },
    clearTimeout(id) { timers.delete(id); },
    setInterval() { return nextTimer++; }, clearInterval() {},
    requestAnimationFrame() { return nextTimer++; }, cancelAnimationFrame() {},
  });
  vm.runInContext(source, context);
  const run = code => vm.runInContext(code, context);
  run('initRefs()');
  function key(key, target = document.body, extra = {}) {
    const event = { key, target, prevented: false, preventDefault() { this.prevented = true; }, ...extra };
    document.handlers.keydown(event);
    return event;
  }
  return { run, document, calls, listeners, timers, api, key, element };
}

function active(h) {
  h.run(`script={title:'Example',word_count:12,source:'# Example',sections:[{name:'Intro'},{name:'Close'}]};
    items=[{text:'First',words:6,section:0,kind:'s'},{text:'Last',words:6,section:1,kind:'s'}];
    mainSentenceItemIdx=[0,1];idx=1;state='run';audioMode=true;trackingReady=true;
    t0=Date.now()-10000;secTimes={0:t0,1:t0+5000};`);
}

function report(extra = {}) {
  return { saved_path: '/private/report.md', sections_covered: ['Intro'], words_delivered: 6, total_words: 12, adherence_pct: 50, coaching: [], verifying: false, verified_by_recording: false, ...extra };
}

function loadNextScript(h) {
  h.run(`loadScript({title:'Next',word_count:3,source:'# Next',sections:[
    {name:'Opening',elements:[{type:'text',sentences:[{text:'Next spoken line.',word_count:3}]}]}
  ]}); startAppleSpeech=()=>{};`);
}

test('Tab and focused controls keep native keyboard behavior', () => {
  const h = harness(); active(h);
  assert.equal(h.key('Tab').prevented, false);
  h.run('globalThis.toggles=0; toggle=()=>toggles++');
  assert.equal(h.key(' ', h.element('button')).prevented, false);
  assert.equal(h.run('toggles'), 0);
  assert.equal(h.key('ArrowUp', h.element('input')).prevented, false);
  assert.equal(h.run('idx'), 1);
  h.key(' ');
  assert.equal(h.run('toggles'), 1);
});

test('manual navigation and direct timer calls cannot start auto-scroll in voice mode', () => {
  const h = harness(); active(h);
  h.run('idx=0;highlight=()=>{};syncTracker=()=>{};startScroll()');
  assert.equal(h.timers.size, 0);
  h.key('ArrowDown');
  assert.equal(h.run('idx'), 1);
  assert.equal(h.timers.size, 0);
});

test('startup is single-flight and blocks replacement until initialized', async () => {
  let resolveInit;
  const h = harness({ init_tracking: () => new Promise(resolve => { resolveInit = resolve; }) });
  active(h);
  h.run("state='loaded';idx=0;startAppleSpeech=()=>{};startFollow=()=>{};startIdleWatch=()=>{};startClock=()=>{};");
  const first = h.run('startSession()');
  const second = h.run('startSession()');
  assert.equal(h.run('state'), 'starting');
  assert.equal(h.run('canReplaceScript()'), false);
  assert.equal(h.calls.filter(([name]) => name === 'init_tracking').length, 1);
  resolveInit();
  await Promise.all([first, second]);
  assert.equal(h.run('state'), 'run');
});

test('speech listeners exist before helper startup and are removed between sessions', async () => {
  const h = harness({ start_speech: (_, api) => api.emit('speech-error', 'permission denied') });
  active(h);
  await h.run('startAppleSpeech()');
  assert.equal(h.run('audioMode'), false);
  assert.match(h.document.getElementById('err').textContent, /permission denied.*Timer mode/);
  assert.equal(h.timers.size, 1);
  for (const name of ['speech', 'track-update', 'other-party', 'speech-warning', 'speech-error']) {
    assert.equal(h.listeners.get(name).size, 0, `${name} listener was disposed`);
  }
});

test('ending during listener setup cannot start a late microphone session', async () => {
  const h = harness(); active(h);
  const starting = h.run('startAppleSpeech()');
  await h.run('stopAppleSpeech()');
  await starting;
  assert.equal(h.calls.some(([name]) => name === 'start_speech'), false);
  assert.equal(h.listeners.get('speech').size, 0);
});

test('paused voice mode tells the reader that the microphone remains active', () => {
  const h = harness(); active(h);
  h.run('toggle()');
  assert.equal(h.run('state'), 'pause');
  assert.match(h.document.getElementById('stLabel').textContent, /microphone still active/);
  assert.equal(h.document.getElementById('btnPause').textContent, 'Resume');
});

test('recording verification cannot be lost if it completes before finish returns', async () => {
  const h = harness({ finish_tracking: (_, api) => {
    api.emit('verification-complete', report({ adherence_pct: 75, words_delivered: 9, verified_by_recording: true }));
    return report({ verifying: true });
  } });
  active(h);
  await h.run('endSession()');
  const card = h.document.getElementById('doneCard');
  assert.match(card.textContent, /Recording checked.*75% estimated script coverage \(9 \/ 12 words in covered lines\)/);
  assert.match(card.textContent, /CloseNo speech evidence/);
  assert.equal(h.listeners.get('verification-complete').size, 0);
  assert.equal(h.run('reportPending'), false);
});

test('verification events are scoped to the saved report', async () => {
  const h = harness({ finish_tracking: () => report({ verifying: true }) });
  active(h);
  await h.run('endSession()');
  const card = h.document.getElementById('doneCard');
  h.api.emit('verification-complete', report({ saved_path: '/other.md', adherence_pct: 100, verified_by_recording: true }));
  assert.match(card.textContent, /Checking full recording.*50%/);
  h.api.emit('verification-failed', { saved_path: '/private/report.md', error: 'recording unavailable' });
  assert.match(card.textContent, /Recording check failed.*recording unavailable/);
  assert.equal(h.listeners.get('verification-failed').size, 0);
});

test('timer-only sessions and save failures never invent cursor-based compliance', async () => {
  const timer = harness(); active(timer); timer.run('trackingReady=false');
  await timer.run('endSession()');
  assert.match(timer.document.getElementById('doneCard').textContent, /no speech evidence.*No delivery coverage/);
  assert.equal(timer.calls.some(([name]) => ['save_compliance', 'finish_tracking', 'get_coaching'].includes(name)), false);
  const failed = harness({ finish_tracking: () => { throw Error('disk full'); } }); active(failed);
  await failed.run('endSession()');
  assert.match(failed.document.getElementById('doneCard').textContent, /Report unavailable.*disk full/);
  assert.equal(failed.calls.some(([name]) => name === 'save_compliance'), false);
});

test('end waits for speech shutdown and blocks Done while the report is pending', async () => {
  let resolveStop;
  const h = harness({ stop_speech: () => new Promise(resolve => { resolveStop = resolve; }), finish_tracking: () => report() });
  active(h);
  const pending = h.run('endSession()');
  assert.equal(h.calls.some(([name]) => name === 'finish_tracking'), false);
  h.run('resetAll()');
  assert.equal(h.run('state'), 'done');
  assert.equal(h.run('script.title'), 'Example');
  resolveStop(); await pending;
  assert.equal(h.calls.some(([name]) => name === 'finish_tracking'), true);
});

test('rapid reset and restart waits for both microphone shutdown and tracker cleanup', async () => {
  let resolveStop, resolveClear;
  const h = harness({
    stop_speech: () => new Promise(resolve => { resolveStop = resolve; }),
    clear_tracking: () => new Promise(resolve => { resolveClear = resolve; }),
  });
  active(h); h.run("state='done';resetAll()");
  loadNextScript(h);
  const starting = h.run('startSession()');
  assert.equal(h.run('state'), 'starting');
  assert.equal(h.document.getElementById('btnStart').disabled, true);
  assert.equal(h.calls.some(([name]) => name === 'init_tracking'), false);
  assert.equal(h.calls.some(([name]) => name === 'clear_tracking'), false);
  resolveStop();
  await new Promise(setImmediate);
  assert.equal(typeof resolveClear, 'function');
  assert.equal(h.calls.some(([name]) => name === 'init_tracking'), false);
  resolveClear(); await starting;
  const ordered = h.calls.map(([name]) => name).filter(name => ['stop_speech', 'clear_tracking', 'init_tracking'].includes(name));
  assert.deepEqual(ordered, ['stop_speech', 'clear_tracking', 'init_tracking']);
  assert.equal(h.run('state'), 'run');
});

test('failed shutdown remains pending across the report and blocks startup until a retry succeeds', async () => {
  let stopAttempts = 0;
  const h = harness({ stop_speech: () => {
    if (++stopAttempts < 3) throw Error('helper still stopping');
  } });
  active(h); await h.run('endSession()');
  assert.equal(h.run('speechStopRequired'), true);
  assert.match(h.document.getElementById('doneCard').textContent, /helper still stopping/);
  assert.equal(h.calls.some(([name]) => name === 'finish_tracking'), false);
  loadNextScript(h);
  await h.run('startSession()');
  assert.equal(h.run('state'), 'loaded');
  assert.equal(h.run('speechStopRequired'), true);
  assert.equal(h.run('timerFallbackUsed'), false);
  assert.equal(h.calls.some(([name]) => name === 'init_tracking'), false);
  assert.match(h.document.getElementById('err').textContent, /previous microphone session could not be closed/);
  await h.run('startSession()');
  assert.equal(h.run('state'), 'run');
  assert.equal(h.run('speechStopRequired'), false);
  assert.equal(h.calls.filter(([name]) => name === 'init_tracking').length, 1);
});

test('tracker initialization failure leaves a retryable loaded script instead of entering timer mode', async () => {
  const h = harness({ init_tracking: () => { throw Error('previous tracker active'); } });
  loadNextScript(h);
  await h.run('startSession()');
  assert.equal(h.run('state'), 'loaded');
  assert.equal(h.run('timerFallbackUsed'), false);
  assert.equal(h.run('resetCleanupRequired'), true);
  assert.equal(h.timers.size, 0);
  assert.equal(h.document.getElementById('btnStart').disabled, false);
  assert.match(h.document.getElementById('err').textContent, /Could not initialize voice tracking/);
});

test('modal focus wraps and background content is inert', () => {
  const h = harness();
  const dialog = h.document.getElementById('helpOverlay');
  const first = h.element('button'), last = h.element('button'); dialog.append(first, last);
  h.run("show('helpOverlay')");
  assert.equal(h.document.getElementById('tp').inert, true);
  last.focus();
  assert.equal(h.key('Tab').prevented, true);
  assert.equal(h.document.activeElement, first);
  h.key('Tab', first, { shiftKey: true });
  assert.equal(h.document.activeElement, last);
  h.run("hide('helpOverlay')");
  assert.equal(h.document.getElementById('tp').inert, false);
});
