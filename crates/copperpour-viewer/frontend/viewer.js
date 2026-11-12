
  const token = new URLSearchParams(location.hash.slice(1)).get('token') || '';
  let selectedFile = null;
  let eventSource = null;
  const handlers = new Map();
  async function request(path, options = {}) {
    const response = await fetch(path, {
      ...options,
      headers: { Authorization: `Bearer ${token}`, ...(options.headers || {}) },
      cache: 'no-store',
    });
    if (!response.ok) throw new Error(`${response.status}: ${await response.text()}`);
    return response;
  }
  async function getSheet(file) {
    return (await request('/sheet?' + new URLSearchParams({ file }))).text();
  }
  const invoke = async (command, args = {}) => {
    if (command === 'get_startup_file') return (await (await request('/state')).json()).root_file;
    if (command === 'open_schematic') {
      const state = await (await request('/state')).json();
      if (args.path !== state.root_file) throw new Error('This viewer is bound to its launch schematic');
      selectedFile = state.root_file;
      return { ...state, active_file: selectedFile, svg: await getSheet(selectedFile), render_errors: state.errors };
    }
    if (command === 'select_sheet') {
      const svg = await getSheet(args.file);
      selectedFile = args.file;
      return svg;
    }
    const endpoint = command === 'refresh' ? '/refresh' : command === 'open_in_kicad' ? '/open-in-kicad' : null;
    if (!endpoint) throw new Error('Unknown viewer command');
    return (await request(endpoint, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ file: selectedFile }) })).json();
  };
  const listen = async (name, handler) => {
    handlers.set(name, handler);
    if (!eventSource) {
      eventSource = new EventSource('/events?' + new URLSearchParams({ token }));
      let generation = 0;
      eventSource.addEventListener('state', async event => {
        const current = ++generation;
        const state = JSON.parse(event.data);
        const emit = (name, payload) => handlers.get(name)?.({ payload });
        if (state.rendering) { emit('change-detected'); return; }
        if (!state.sheets.some(sheet => sheet.file === selectedFile)) selectedFile = state.root_file;
        emit('sheets-updated', { sheets: state.sheets, active_file: selectedFile });
        try {
          const svg = await getSheet(selectedFile);
          if (current === generation) emit('schematic-updated', svg);
        } catch (error) { emit('viewer-error', String(error)); }
        state.errors.forEach(error => emit('viewer-error', error));
      });
      eventSource.onerror = () => handlers.get('viewer-error')?.({ payload: 'Viewer connection lost; reconnecting…' });
      window.addEventListener('pagehide', () => eventSource.close());
      window.addEventListener('pageshow', event => { if (event.persisted) location.reload(); });
    }
    return () => handlers.delete(name);
  };

  const MIN_SCALE = 0.05, MAX_SCALE = 40;

  let scale = 1;
  let panX = 0, panY = 0;
  let isDragging = false;
  let dragStartX, dragStartY;
  let darkTheme = true;

  const viewer = document.getElementById('viewer');
  const container = document.getElementById('svg-container');
  const zoomDisplay = document.getElementById('zoom-display');
  const statusEl = document.getElementById('status');
  const flashEl = document.getElementById('flash');
  const emptyState = document.getElementById('empty-state');
  const sheetSelector = document.getElementById('sheet-selector');
  const filenameEl = document.getElementById('filename');
  const loadingOverlay = document.getElementById('loading-overlay');
  const loadingText = document.getElementById('loading-text');

  // Unmistakable, centered loading state — a small "Rendering..." label in
  // the corner reads as "the app might be hung" on a large, slow-to-render
  // hierarchical design (real-world case: ~20 sheets took long enough that
  // it looked frozen). This can't be missed the same way.
  function showLoading(text) {
    loadingText.textContent = text || 'Rendering…';
    loadingOverlay.style.display = '';
    emptyState.style.display = 'none';
  }
  function hideLoading() {
    loadingOverlay.style.display = 'none';
  }

  function clampScale(s) {
    return Math.min(MAX_SCALE, Math.max(MIN_SCALE, s));
  }

  // Zoom resizes the SVG rather than scaling it with CSS: WebKitGTK scales a
  // transformed subtree as a bitmap drawn at layout size, which for an A3
  // sheet is ~420px wide, so no zoom level ever made text readable.
  function updateTransform() {
    container.style.transform = `translate(${panX}px, ${panY}px)`;
    const svgEl = currentSvg();
    const size = svgEl && sheetSize(svgEl);
    if (size) {
      svgEl.setAttribute('width', String(size.w * scale));
      svgEl.setAttribute('height', String(size.h * scale));
      if (svgFrame) {
        svgFrame.style.width = `${size.w * scale}px`;
        svgFrame.style.height = `${size.h * scale}px`;
      }
    }
    zoomDisplay.textContent = Math.round(scale * 100) + '%';
  }

  // The sheet's size in viewBox units, which are CSS px at 100% zoom.
  function sheetSize(svgEl) {
    const vb = svgEl.getAttribute('viewBox');
    if (!vb) return null;
    const [, , w, h] = vb.split(/\s+/).map(Number);
    return w && h ? { w, h } : null;
  }

  function applySvgTheme() {
    const svgEl = currentSvg();
    if (svgEl) {
      svgEl.style.filter = darkTheme ? 'invert(0.85) hue-rotate(180deg)' : 'none';
    }
  }

  let svgFrame = null;
  function currentSvg() {
    return svgFrame?.contentDocument?.querySelector('svg');
  }
  let svgQueue = Promise.resolve();
  function setSvg(svgContent) {
    svgQueue = svgQueue.then(() => replaceSvg(svgContent));
    return svgQueue;
  }
  async function replaceSvg(svgContent) {
    if (!svgFrame) {
      svgFrame = document.createElement('iframe');
      // Same-origin permits the trusted parent to adjust SVG dimensions. Never
      // allow scripts: the SVG must not access the parent's launch credential.
      svgFrame.setAttribute('sandbox', 'allow-same-origin');
      svgFrame.setAttribute('title', 'Schematic');
      svgFrame.style.cssText = 'border:0;pointer-events:none;display:block';
      container.replaceChildren(svgFrame);
    }
    await new Promise(resolve => {
      svgFrame.onload = resolve;
      svgFrame.srcdoc = '<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; img-src data:; base-uri \'none\'; form-action \'none\'"><style>html,body{margin:0;overflow:hidden;background:transparent}</style>' + svgContent;
    });
    emptyState.style.display = 'none';
    hideLoading();

    const svgEl = currentSvg();
    if (svgEl) {
      svgEl.style.background = 'transparent';
      // kicad-cli's <title> ("SVG Image created as …") became a native
      // tooltip over the whole sheet, and on Linux it left a gray ghost.
      svgEl.querySelector(':scope > title')?.remove();
      // Ensure a viewBox so fitToPage can compute dimensions
      if (!svgEl.getAttribute('viewBox') && svgEl.getAttribute('width')) {
        const w = parseFloat(svgEl.getAttribute('width'));
        const h = parseFloat(svgEl.getAttribute('height'));
        if (w && h) svgEl.setAttribute('viewBox', `0 0 ${w} ${h}`);
      }
      // kicad-cli's width/height carry real-world units (e.g. "419.9890mm"
      // for an A3 sheet). The browser renders the SVG's on-screen size from
      // those — at ~3.78 CSS px/mm, an A3 sheet is natively ~1587x1122px —
      // but fitToPage()'s scale math assumes 1 viewBox unit = 1 CSS pixel.
      // Left alone, that mismatch makes "Fit" wildly over-zoom on anything
      // bigger than a postage stamp. updateTransform() sets width/height to
      // the viewBox's own numeric units times the zoom, so the two always
      // agree, regardless of sheet size, and a re-render keeps the zoom.
    }
    updateTransform();
    applySvgTheme();
  }

  // `sticky` keeps the message up until the next flash replaces it — for
  // in-progress states (re-rendering) rather than one-shot notifications.
  // The single shared timer matters: without it, an earlier flash's
  // hide-timeout fires late and wipes whatever message replaced it.
  let flashTimer = null;
  function flash(msg, isError, sticky) {
    flashEl.textContent = msg || 'Updated';
    flashEl.classList.toggle('error', !!isError);
    flashEl.classList.add('visible');
    if (flashTimer) {
      clearTimeout(flashTimer);
      flashTimer = null;
    }
    if (!sticky) {
      flashTimer = setTimeout(() => flashEl.classList.remove('visible'), isError ? 4000 : 2000);
    }
  }

  function fitToPage() {
    const svgEl = currentSvg();
    const size = svgEl && sheetSize(svgEl);
    if (!size) return;
    const { w: svgW, h: svgH } = size;
    const viewW = viewer.clientWidth;
    const viewH = viewer.clientHeight;
    scale = clampScale(Math.min(viewW / svgW, viewH / svgH) * 0.95);
    panX = (viewW - svgW * scale) / 2;
    panY = (viewH - svgH * scale) / 2;
    updateTransform();
  }

  // Zoom keeping the sheet point under (x, y) fixed, in viewer coordinates.
  function zoomAt(factor, x, y) {
    const newScale = clampScale(scale * factor);
    if (newScale === scale) return;
    panX = x - (x - panX) * (newScale / scale);
    panY = y - (y - panY) * (newScale / scale);
    scale = newScale;
    updateTransform();
  }

  // Buttons and keys zoom about the window center; anchored at the sheet's
  // corner, which is off-screen once zoomed in, they slid the sheet away.
  function zoomIn() { zoomAt(1.25, viewer.clientWidth / 2, viewer.clientHeight / 2); }
  function zoomOut() { zoomAt(0.8, viewer.clientWidth / 2, viewer.clientHeight / 2); }

  function toggleTheme() {
    darkTheme = !darkTheme;
    document.body.classList.toggle('light', !darkTheme);
    applySvgTheme();
  }

  async function openInKicad() {
    try {
      await invoke('open_in_kicad');
      statusEl.textContent = 'KiCad launch requested';
    } catch (e) {
      statusEl.textContent = 'Error: ' + e;
      flash(String(e), true);
    }
  }

  // Mouse wheel zoom (centered on cursor)
  viewer.addEventListener('wheel', (e) => {
    e.preventDefault();
    const rect = viewer.getBoundingClientRect();
    zoomAt(e.deltaY < 0 ? 1.1 : 0.9, e.clientX - rect.left, e.clientY - rect.top);
  }, { passive: false });

  // Mouse drag pan
  viewer.addEventListener('mousedown', (e) => {
    isDragging = true;
    dragStartX = e.clientX - panX;
    dragStartY = e.clientY - panY;
    viewer.classList.add('dragging');
  });

  window.addEventListener('mousemove', (e) => {
    if (!isDragging) return;
    panX = e.clientX - dragStartX;
    panY = e.clientY - dragStartY;
    updateTransform();
  });

  window.addEventListener('mouseup', () => {
    isDragging = false;
    viewer.classList.remove('dragging');
  });

  // Keyboard shortcuts
  window.addEventListener('keydown', (e) => {
    if (e.key === '+' || e.key === '=') zoomIn();
    else if (e.key === '-') zoomOut();
    else if (e.key === '0') fitToPage();
    else if (e.key === 'r' || e.key === 'R') manualRefresh();
  });

  async function manualRefresh() {
    try {
      statusEl.textContent = 'Refreshing...';
      showLoading('Refreshing...');
      const svg = await invoke('refresh');
      await setSvg(svg);
      flash('Refreshed');
      statusEl.textContent = 'Ready';
    } catch (e) {
      hideLoading();
      statusEl.textContent = 'Error: ' + e;
      flash(String(e), true);
    }
  }

  // ─── Sheet selector ────────────────────────────────────────────────────

  // Depth-indented flat list, in the same depth-first order the backend's
  // walk (and sch_hierarchy's get_sheet_hierarchy/renumber_sheet_pages)
  // already use — not a collapsible tree, see the design note for why.
  function populateSheetSelector(sheets, activeFile) {
    sheetSelector.innerHTML = '';
    if (!sheets || sheets.length <= 1) {
      sheetSelector.hidden = true;
      return;
    }
    for (const s of sheets) {
      const opt = document.createElement('option');
      opt.value = s.file;
      // Plain spaces collapse to a single space inside <option> text (normal HTML whitespace rules apply even here), so indentation below uses non-breaking spaces, not regular ones, to actually show up.
      const indent = '    '.repeat(s.depth);
      const marker = s.depth > 0 ? '└─ ' : ''; // box-drawing tree marker
      opt.textContent = indent + marker + s.name;
      sheetSelector.appendChild(opt);
    }
    sheetSelector.value = activeFile;
    sheetSelector.hidden = false;
  }

  async function onSheetSelected() {
    const file = sheetSelector.value;
    try {
      statusEl.textContent = 'Loading sheet...';
      showLoading('Loading sheet...');
      const svg = await invoke('select_sheet', { file });
      await setSvg(svg);
      fitToPage();
      statusEl.textContent = 'Watching for changes...';
    } catch (e) {
      hideLoading();
      statusEl.textContent = 'Error: ' + e;
      flash(String(e), true);
    }
  }

  // Surface event setup failures in the viewer.
  function listenOrWarn(name, handler) {
    listen(name, handler).catch((e) => {
      statusEl.textContent = 'Event wiring failed';
      flash('Event wiring failed for "' + name + '": ' + e, true);
    });
  }

  listenOrWarn('schematic-updated', async (event) => {
    await setSvg(event.payload);
    flash('File changed — updated');
    statusEl.textContent = 'Live';
  });

  // Fired whenever the watcher re-walks the tree — keeps the selector in
  // sync without requiring the viewer to be reopened. `active_file` reflects
  // any backend fallback-to-root (e.g. the selected sheet was deleted).
  // Emitted at the end of every render cycle (whether or not the active
  // sheet changed) — so it also retires the sticky "re-rendering" flash.
  // The retirement is deferred a beat: when the active sheet WAS
  // re-rendered, schematic-updated lands right behind this event and takes
  // over the flash — showing "Re-render complete" for a split second first
  // just reads as flicker. It only appears when it's genuinely the final
  // word (the edit touched sheets other than the one on screen).
  listenOrWarn('sheets-updated', (event) => {
    populateSheetSelector(event.payload.sheets, event.payload.active_file);
    statusEl.textContent = 'Live';
    setTimeout(() => {
      if (flashEl.classList.contains('visible') && flashEl.textContent === RENDERING_MSG) {
        flash('Re-render complete');
      }
    }, 150);
  });

  // Fired the instant the backend's watcher notices a relevant file change,
  // before any rendering starts (which can take a while on a big design) -
  // makes "watcher never fired" visibly different from "render failed".
  const RENDERING_MSG = 'Change detected - re-rendering...';
  listenOrWarn('change-detected', () => {
    statusEl.textContent = RENDERING_MSG;
    // Sticky: stays up for the whole render (can be several seconds on a
    // big design) so the user watches a state, not a vanishing blip. The
    // sheets-updated handler below retires it when the cycle finishes.
    flash(RENDERING_MSG, false, true);
  });

  listenOrWarn('viewer-error', (event) => {
    statusEl.textContent = 'Error';
    flash(String(event.payload), true);
  });

  // Open the launch root and populate its eagerly rendered sheet hierarchy.
  window.loadSchematic = async function(path) {
    try {
      filenameEl.textContent = path.split(/[\\/]/).pop();
      statusEl.textContent = 'Rendering...';
      showLoading('Rendering... (a large hierarchical design can take a while — every sheet is rendered up front)');
      const result = await invoke('open_schematic', { path });
      populateSheetSelector(result.sheets, result.active_file);
      await setSvg(result.svg);
      fitToPage();
      if (result.render_errors && result.render_errors.length) {
        flash(`${result.render_errors.length} sheet(s) failed to render`, true);
      }
      statusEl.textContent = 'Watching for changes...';
    } catch (e) {
      hideLoading();
      statusEl.textContent = 'Error: ' + e;
      flash(String(e), true);
      console.error(e);
    }
  };

  // Startup: ask the root-bound browser service for its launch file.
  (async function init() {
    try {
      const startupFile = await invoke('get_startup_file');
      if (startupFile) {
        await window.loadSchematic(startupFile);
      } else {
        statusEl.textContent = 'No launch schematic available';
      }
    } catch (e) {
      console.error(e);
    }
  })();

  const actions = { zoomIn, zoomOut, fitToPage, manualRefresh, toggleTheme, openInKicad };
  document.querySelectorAll('[data-action]').forEach(button => button.addEventListener('click', actions[button.dataset.action]));
  sheetSelector.addEventListener('change', onSheetSelected);
  window.addEventListener('dragover', event => event.preventDefault());
  window.addEventListener('drop', event => event.preventDefault());
