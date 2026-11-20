use std::path::Path;
use std::process::Command;

const HARNESS_JS: &str = r#"
const fs = require('fs');
const vm = require('vm');
const assert = require('assert');

const viewerJsPath = process.argv[1];
const scenario = process.argv[2];
const source = fs.readFileSync(viewerJsPath, 'utf8');

function createHarness({ width, height, withResizeObserver = true }) {
  let viewportW = width;
  let viewportH = height;

  const elements = new Map();
  const actionButtons = [];
  const windowListeners = new Map();
  const resizeObservers = [];

  function makeElement(id, tag = 'div') {
    const listeners = new Map();
    const attrs = new Map();
    const classes = new Set();
    const children = [];
    const el = {
      id,
      tagName: tag.toUpperCase(),
      style: {},
      dataset: {},
      hidden: false,
      value: '',
      textContent: '',
      _innerHTML: '',
      get innerHTML() { return this._innerHTML; },
      set innerHTML(v) { this._innerHTML = v; children.length = 0; },
      classList: {
        add(c) { classes.add(c); },
        remove(c) { classes.delete(c); },
        toggle(c, force) {
          if (force === undefined) {
            if (classes.has(c)) classes.delete(c);
            else classes.add(c);
          } else if (force) classes.add(c);
          else classes.delete(c);
        },
        contains(c) { return classes.has(c); },
      },
      addEventListener(type, fn) {
        if (!listeners.has(type)) listeners.set(type, []);
        listeners.get(type).push(fn);
      },
      dispatch(type, event = {}) {
        event.preventDefault = event.preventDefault || (() => {});
        for (const fn of listeners.get(type) || []) fn(event);
      },
      setAttribute(k, v) { attrs.set(k, String(v)); },
      getAttribute(k) { return attrs.has(k) ? attrs.get(k) : null; },
      appendChild(child) { children.push(child); return child; },
      replaceChildren(...nodes) { children.length = 0; children.push(...nodes); },
      querySelector() { return null; },
      getBoundingClientRect() {
        return { left: 0, top: 0, width: viewportW, height: viewportH };
      },
    };
    if (id === 'viewer') {
      Object.defineProperty(el, 'clientWidth', { get: () => viewportW });
      Object.defineProperty(el, 'clientHeight', { get: () => viewportH });
    }
    if (id) elements.set(id, el);
    return el;
  }

  for (const id of [
    'viewer', 'svg-container', 'zoom-display', 'status', 'flash',
    'empty-state', 'sheet-selector', 'filename', 'loading-overlay', 'loading-text',
  ]) {
    makeElement(id);
  }
  for (const action of ['zoomIn', 'zoomOut', 'fitToPage', 'manualRefresh', 'toggleTheme', 'openInKicad']) {
    const btn = makeElement('', 'button');
    btn.dataset.action = action;
    actionButtons.push(btn);
  }

  const document = {
    body: makeElement('body', 'body'),
    getElementById(id) { return elements.get(id) || null; },
    querySelectorAll(sel) { return sel === '[data-action]' ? actionButtons : []; },
    createElement(tag) {
      const el = makeElement('', tag);
      if (tag === 'iframe') {
        let currentDoc = null;
        Object.defineProperty(el, 'contentDocument', { get: () => currentDoc });
        Object.defineProperty(el, 'srcdoc', {
          get() { return this._srcdoc || ''; },
          set(html) {
            this._srcdoc = html;
            const vbMatch = html.match(/viewBox="([^"]+)"/);
            const svgEl = makeElement('', 'svg');
            if (vbMatch) svgEl.setAttribute('viewBox', vbMatch[1]);
            currentDoc = {
              querySelector(sel) { return sel === 'svg' ? svgEl : null; },
            };
            queueMicrotask(() => { if (typeof el.onload === 'function') el.onload(); });
          },
        });
      }
      return el;
    },
  };

  const windowObj = {
    addEventListener(type, fn) {
      if (!windowListeners.has(type)) windowListeners.set(type, []);
      windowListeners.get(type).push(fn);
    },
    dispatch(type, event = {}) {
      event.preventDefault = event.preventDefault || (() => {});
      for (const fn of windowListeners.get(type) || []) fn(event);
    },
  };

  class FakeResizeObserver {
    constructor(cb) {
      this.cb = cb;
      this.targets = new Set();
      resizeObservers.push(this);
    }
    observe(target) {
      this.targets.add(target);
    }
    unobserve(target) { this.targets.delete(target); }
    disconnect() { this.targets.clear(); }
  }

  class FakeEventSource {
    constructor() {}
    addEventListener() {}
    close() {}
  }

  const SVG_A3 = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 419.9890 297.0022"></svg>';
  const statePayload = {
    root_file: '/proj/root.kicad_sch',
    sheets: [
      { name: 'root', file: '/proj/root.kicad_sch', depth: 0 },
      { name: 'child', file: '/proj/child.kicad_sch', depth: 1 },
    ],
    errors: [],
  };

  async function fakeFetch(path) {
    if (path === '/state') {
      return { ok: true, json: async () => statePayload, text: async () => JSON.stringify(statePayload) };
    }
    if (path.startsWith('/sheet?')) {
      return { ok: true, text: async () => SVG_A3 };
    }
    throw new Error('Unexpected fetch: ' + path);
  }

  const sandbox = {
    document,
    window: windowObj,
    location: { hash: '#token=test', reload() {} },
    URLSearchParams,
    EventSource: FakeEventSource,
    fetch: fakeFetch,
    console,
    setTimeout,
    clearTimeout,
    Promise,
    Map,
    Set,
    Math,
    Number,
    String,
    JSON,
    Error,
    parseFloat,
  };
  if (withResizeObserver) {
    sandbox.ResizeObserver = FakeResizeObserver;
  }

  vm.runInNewContext(source, sandbox, { filename: 'viewer.js' });

  async function flush() {
    for (let i = 0; i < 10; i++) {
      await new Promise(r => setTimeout(r, 0));
    }
  }

  function resize(w, h) {
    viewportW = w;
    viewportH = h;
    if (withResizeObserver) {
      const viewerEl = elements.get('viewer');
      for (const ro of resizeObservers) {
        if (ro.targets.has(viewerEl)) {
          ro.cb([{ target: viewerEl, contentRect: { width: w, height: h } }], ro);
        }
      }
    } else {
      windowObj.dispatch('resize');
    }
  }

  function clickAction(action) {
    const btn = actionButtons.find(b => b.dataset.action === action);
    assert(btn, 'missing button ' + action);
    btn.dispatch('click');
  }

  return {
    elements,
    windowObj,
    flush,
    resize,
    clickAction,
    zoomText: () => elements.get('zoom-display').textContent,
    transform: () => elements.get('svg-container').style.transform,
  };
}

function expectedZoom(viewW, viewH) {
  const s = Math.min(40, Math.max(0.05, Math.min(viewW / 419.9890, viewH / 297.0022) * 0.95));
  return Math.round(s * 100) + '%';
}

function expectedTransform(viewW, viewH) {
  const s = Math.min(40, Math.max(0.05, Math.min(viewW / 419.9890, viewH / 297.0022) * 0.95));
  const panX = (viewW - 419.9890 * s) / 2;
  const panY = (viewH - 297.0022 * s) / 2;
  return `translate(${panX}px, ${panY}px)`;
}

(async () => {
  if (scenario === 'small_initial_viewport') {
    const h = createHarness({ width: 25, height: 25 });
    await h.flush();
    assert.strictEqual(h.zoomText(), expectedZoom(25, 25)); // 6%
    h.resize(1200, 800);
    assert.strictEqual(h.zoomText(), expectedZoom(1200, 800));
    assert.strictEqual(h.transform(), expectedTransform(1200, 800));
  } else if (scenario === 'zero_initial_viewport') {
    const h = createHarness({ width: 0, height: 0 });
    await h.flush();
    assert.strictEqual(h.zoomText(), '100%', '0x0 viewport must not pin zoom to 5%');
    assert.strictEqual(h.transform(), 'translate(0px, 0px)', '0x0 viewport must not set negative pan');
    h.resize(1000, 700);
    assert.strictEqual(h.zoomText(), expectedZoom(1000, 700));
    assert.strictEqual(h.transform(), expectedTransform(1000, 700));
  } else if (scenario === 'preserves_after_zoom') {
    const h = createHarness({ width: 1000, height: 700 });
    await h.flush();
    assert.strictEqual(h.zoomText(), expectedZoom(1000, 700));
    h.elements.get('viewer').dispatch('wheel', { deltaY: -100, clientX: 500, clientY: 350 });
    const zoomedText = h.zoomText();
    const zoomedTransform = h.transform();
    assert.notStrictEqual(zoomedText, expectedZoom(1000, 700));
    h.resize(1400, 900);
    assert.strictEqual(h.zoomText(), zoomedText, 'resize after wheel zoom must preserve zoom');
    assert.strictEqual(h.transform(), zoomedTransform, 'resize after wheel zoom must preserve pan');
  } else if (scenario === 'preserves_after_drag_pan') {
    const h = createHarness({ width: 1000, height: 700 });
    await h.flush();
    h.elements.get('viewer').dispatch('mousedown', { clientX: 100, clientY: 100 });
    h.windowObj.dispatch('mousemove', { clientX: 140, clientY: 120 });
    h.windowObj.dispatch('mouseup');
    const pannedText = h.zoomText();
    const pannedTransform = h.transform();
    h.resize(1400, 900);
    assert.strictEqual(h.zoomText(), pannedText, 'resize after drag pan must preserve zoom');
    assert.strictEqual(h.transform(), pannedTransform, 'resize after drag pan must preserve pan');
  } else if (scenario === 'plain_click_keeps_auto_fit') {
    const h = createHarness({ width: 1000, height: 700 });
    await h.flush();
    h.elements.get('viewer').dispatch('mousedown', { clientX: 100, clientY: 100 });
    h.windowObj.dispatch('mousemove', { clientX: 100, clientY: 100 });
    h.windowObj.dispatch('mouseup');
    h.resize(1400, 900);
    assert.strictEqual(h.zoomText(), expectedZoom(1400, 900), 'plain click must not disable auto-fit');
    assert.strictEqual(h.transform(), expectedTransform(1400, 900));
  } else if (scenario === 'fit_reenables_auto_fit') {
    const h = createHarness({ width: 1000, height: 700 });
    await h.flush();
    h.clickAction('zoomIn');
    const zoomed = h.zoomText();
    h.resize(1200, 800);
    assert.strictEqual(h.zoomText(), zoomed);
    h.clickAction('fitToPage');
    assert.strictEqual(h.zoomText(), expectedZoom(1200, 800));
    h.resize(1400, 900);
    assert.strictEqual(h.zoomText(), expectedZoom(1400, 900), 'Fit button must re-enable auto-fit');
    // Also check '0' key after pan
    h.elements.get('viewer').dispatch('mousedown', { clientX: 50, clientY: 50 });
    h.windowObj.dispatch('mousemove', { clientX: 90, clientY: 80 });
    h.windowObj.dispatch('mouseup');
    h.windowObj.dispatch('keydown', { key: '0' });
    h.resize(800, 600);
    assert.strictEqual(h.zoomText(), expectedZoom(800, 600), '0 key must re-enable auto-fit');
  } else if (scenario === 'sheet_switch_reenables_auto_fit') {
    const h = createHarness({ width: 1000, height: 700 });
    await h.flush();
    h.clickAction('zoomIn');
    const sel = h.elements.get('sheet-selector');
    sel.value = '/proj/child.kicad_sch';
    sel.dispatch('change');
    await h.flush();
    h.resize(1400, 900);
    assert.strictEqual(h.zoomText(), expectedZoom(1400, 900), 'switching sheets must re-enable auto-fit');
  } else if (scenario === 'window_resize_fallback') {
    const h = createHarness({ width: 25, height: 25, withResizeObserver: false });
    await h.flush();
    assert.strictEqual(h.zoomText(), expectedZoom(25, 25));
    h.resize(1200, 800);
    assert.strictEqual(h.zoomText(), expectedZoom(1200, 800));
  } else {
    throw new Error('Unknown scenario: ' + scenario);
  }
})().catch(err => {
  console.error(err);
  process.exit(1);
});
"#;

fn run_viewer_scenario(scenario: &str) {
    let viewer_js = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/viewer.js");
    let output = match Command::new("node")
        .arg("-e")
        .arg(HARNESS_JS)
        .arg(&viewer_js)
        .arg(scenario)
        .output()
    {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if std::env::var_os("CI").is_some() {
                panic!("node is required in CI to run viewer.js tests: {err}");
            }
            eprintln!("skipping viewer.js test {scenario}: node not found on PATH");
            return;
        }
        Err(err) => panic!("failed to spawn node for {scenario}: {err}"),
    };
    assert!(
        output.status.success(),
        "viewer.js scenario `{scenario}` failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn viewer_refits_after_small_initial_viewport() {
    run_viewer_scenario("small_initial_viewport");
}

#[test]
fn viewer_refits_after_zero_initial_viewport() {
    run_viewer_scenario("zero_initial_viewport");
}

#[test]
fn viewer_preserves_view_after_user_zoom() {
    run_viewer_scenario("preserves_after_zoom");
}

#[test]
fn viewer_preserves_view_after_user_drag_pan() {
    run_viewer_scenario("preserves_after_drag_pan");
}

#[test]
fn viewer_plain_click_without_drag_keeps_auto_fit() {
    run_viewer_scenario("plain_click_keeps_auto_fit");
}

#[test]
fn viewer_fit_button_and_zero_key_reenable_auto_fit() {
    run_viewer_scenario("fit_reenables_auto_fit");
}

#[test]
fn viewer_sheet_switch_reenables_auto_fit() {
    run_viewer_scenario("sheet_switch_reenables_auto_fit");
}

#[test]
fn viewer_window_resize_fallback_when_resize_observer_absent() {
    run_viewer_scenario("window_resize_fallback");
}
