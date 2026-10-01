async page => {
  const cfg = __FIXTURE_CONFIG__;
  const verify = (condition, label) => { if (!condition) throw new Error(label); };
  const checks = [], metrics = { maxRows: 0, maxPages: 0, maxDom: 0, maxCursorSlots: 0, forwardLoads: 0, reverseLoads: 0 };
  let phase = 'login';
  const api = async (path, method = 'GET', body) => page.evaluate(async ({ path, method, body }) => {
    const csrf = (await (await fetch('/api/auth/status')).json()).csrf_token;
    const response = await fetch(path, { method, headers: { 'Content-Type': 'application/json', ...(csrf ? { 'x-csrf-token': csrf } : {}) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    return { status: response.status, body: await response.json() };
  }, { path, method, body });
  const login = async (username, profile) => {
    await page.context().clearCookies();
    verify((await api('/api/auth/login', 'POST', { username, password: cfg.password })).status === 200, 'Real login');
    verify((await api('/api/auth/profile', 'POST', { profile_id: profile })).status === 200, 'Real profile');
    await page.goto(cfg.origin);
    await page.getByRole('heading', { name: 'Account', exact: true }).waitFor();
  };
  const navigate = async name => {
    const opener = page.getByRole('button', { name: 'Open navigation', exact: true });
    if (await opener.isVisible()) await opener.click();
    await page.getByRole('navigation', { name: 'Main navigation' }).getByRole('button', { name, exact: true }).click();
  };
  const region = page.getByRole('region', { name: 'Unmatched provider titles' });
  const idle = async () => {
    await page.waitForFunction(() => {
      const r = document.querySelector('.admin-match-scroll');
      return r && Number(r.dataset.retainedRows) > 0 && !r.querySelector('[role=status]');
    });
  };
  const measure = async () => {
    const state = await region.evaluate(el => ({ rows: Number(el.dataset.retainedRows), pages: Number(el.dataset.retainedPages),
      slots: Number(el.dataset.cursorSlots), extent: Number(el.dataset.visitedExtent), base: Number(el.dataset.windowBase),
      dom: el.querySelectorAll('.admin-match-row').length, top: el.scrollTop, regionHeight: el.getBoundingClientRect().height,
      rowHeight: el.querySelector('.admin-match-row')?.getBoundingClientRect().height }));
    verify(state.rows <= 150 && state.pages <= 3 && state.slots <= 9 && state.dom <= 20, 'Retained model/metadata/DOM bounds');
    verify(state.rows > 0 && state.dom > 0, 'Measured retained window missing');
    metrics.maxRows = Math.max(metrics.maxRows, state.rows); metrics.maxPages = Math.max(metrics.maxPages, state.pages);
    metrics.maxDom = Math.max(metrics.maxDom, state.dom); metrics.maxCursorSlots = Math.max(metrics.maxCursorSlots, state.slots);
    metrics.regionHeight = state.regionHeight; metrics.rowHeight = state.rowHeight;
    verify(Math.abs(state.regionHeight - page.viewportSize().height * .6) <= 1, 'Region is not60vh');
    verify(state.rowHeight === (page.viewportSize().width < 768 ? 184 : 112), 'Fixed rendered row height');
    return state;
  };
  const choose = async value => {
    await page.locator('select').first().selectOption(value);
    await idle();
  };
  try {
    await login('qa_member', 2);
    let failProviders = true;
    const providerRoute = async route => {
      if (route.request().url().includes('cursor=') && failProviders) { failProviders = false; await route.abort('failed'); }
      else await route.continue();
    };
    await page.route('**/api/v2/iptv/connections?*', providerRoute);
    await navigate('VOD matches');
    await page.getByRole('button', { name: 'Try again', exact: true }).waitFor();
    verify(await page.evaluate(() => document.querySelector('select').options.length) === 201, 'Provider failure lost first200 metadata');
    await page.unroute('**/api/v2/iptv/connections?*', providerRoute);
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await page.waitForFunction(() => document.querySelectorAll('select')[0]?.options.length === 209);
    const values = await page.evaluate(() => Array.from(document.querySelector('select').options, o => o.value));
    verify(values.includes('605') && !values.includes('201') && values.length === 209, 'All208 owned providers and raw identities; option count=' + values.length + '; first=' + values.slice(0, 5).join(',') + '; last=' + values.slice(-5).join(','));
    await choose('605');
    await page.getByText('Tail provider title', { exact: true }).waitFor();
    checks.push('All208 owned provider options; selectable605 tail, foreign201 absent');
    checks.push('Second provider metadata page transport failure retains first200 options; Try again completes208');
    await choose('101');
    await page.waitForFunction(() => document.querySelector('.admin-match-row')?.dataset.matchId.startsWith('vod:101:'));
    phase = 'full forward eviction';
    const rowHeight = page.viewportSize().width < 768 ? 184 : 112;
    const skipped = cfg.skipped_ids ?? [];
    const expectedId = offset => {
      let index = offset + 1;
      for (const skip of skipped) if (index >= skip) index++;
      return `vod:101:${String(index).padStart(6, '0')}`;
    };
    const validatePage = (body, offset) => {
      verify(body.items.length > 0 && body.items.length <= 50, 'Actual frontend response page bound');
      for (let index = 0; index < body.items.length; index++) verify(body.items[index].vod_id === expectedId(offset + index), 'Frontend traversal omission/duplicate/order at offset' + (offset + index));
    };
    validatePage((await api('/api/v2/iptv/matches?provider_id=101&limit=50')).body, 0);
    const expectedTotal = 99999 - skipped.length;
    for (let step = 0; step < 2000; step++) {
      const old = await measure();
      if (old.extent === expectedTotal) break;
      const incoming = page.waitForResponse(r => r.url().includes('/api/v2/iptv/matches?') && r.url().includes('provider_id=101') && r.url().includes('cursor=') && r.ok());
      await region.evaluate((el, target) => { el.scrollTop = target; }, (old.base + old.rows - 5) * rowHeight);
      validatePage(await (await incoming).json(), old.base + old.rows);
      await page.waitForFunction(extent => Number(document.querySelector('.admin-match-scroll').dataset.visitedExtent) > extent, old.extent);
      await idle();
      metrics.forwardLoads++;
    }
    const far = await measure();
    verify(far.extent === expectedTotal && far.base > 99000, 'Full frontend catalog end missing');
    metrics.forwardTitles = far.extent;
    phase = 'full reverse evicted-page refill';
    for (let step = 0; step < 2000; step++) {
      const old = await measure();
      if (old.base === 0) break;
      const incoming = page.waitForResponse(r => r.url().includes('/api/v2/iptv/matches?') && r.url().includes('provider_id=101') && r.url().includes('cursor=') && r.ok());
      await region.evaluate((el, target) => { el.scrollTop = target; }, (old.base + 4) * rowHeight);
      validatePage(await (await incoming).json(), old.base - 50);
      await page.waitForFunction(base => Number(document.querySelector('.admin-match-scroll').dataset.windowBase) < base, old.base);
      await idle();
      metrics.reverseLoads++;
    }
    await region.evaluate(el => { el.scrollTop = 0; });
    await page.waitForFunction(() => Number(document.querySelector('.admin-match-scroll').dataset.windowBase) === 0);
    metrics.reverseTitles = expectedTotal;
    await page.locator('[data-match-id="vod:101:000001"]').waitFor();
    verify(await page.locator('[data-match-id="vod:101:000001"]').count() === 1, 'Evicted initial page identity did not reload');
    await measure();
    checks.push('Full synthetic100k catalog frontend forward/end/back-to-start traversal; every real incoming page identity/order verified; <=150 retained rows,3 pages,9 cursor slots,20 DOM each page');
    phase = 'evicted forward spacer jump';
    let releaseJump, seenJump;
    const heldJump = new Promise(resolve => { releaseJump = resolve; });
    const receivedJump = new Promise(resolve => { seenJump = resolve; });
    let firstJump = true;
    const jumpRoute = async route => {
      if (firstJump && route.request().url().includes('cursor=')) {
        firstJump = false; seenJump(); await heldJump; await route.abort('failed');
      } else await route.continue();
    };
    await page.route('**/api/v2/iptv/matches?*', jumpRoute);
    const beforeJump = await measure();
    await region.evaluate((el, top) => { el.scrollTop = top; }, 500 * rowHeight);
    await receivedJump;
    const pendingJump = await measure();
    verify(pendingJump.extent === beforeJump.extent && pendingJump.rows === beforeJump.rows, 'Pending spacer jump changed extent/window');
    verify(await region.evaluate(el => {
      const r = el.getBoundingClientRect();
      return Array.from(el.querySelectorAll('.admin-match-row')).some(row => { const b = row.getBoundingClientRect(); return b.bottom > r.top && b.top < r.bottom; });
    }), 'Pending evicted spacer jump displayed blank region');
    releaseJump();
    await page.getByRole('button', { name: 'Try again', exact: true }).waitFor();
    const failedJump = await measure();
    verify(failedJump.extent === beforeJump.extent && failedJump.rows === beforeJump.rows, 'Failed spacer jump changed extent/window');
    await page.unroute('**/api/v2/iptv/matches?*', jumpRoute);
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await page.waitForFunction(() => {
      const r = document.querySelector('.admin-match-scroll');
      return Number(r.dataset.windowBase) <= 500 && Number(r.dataset.windowBase) + Number(r.dataset.retainedRows) > 500 && !r.querySelector('[role=status]');
    });
    await measure();
    await region.evaluate(el => { el.scrollTop = 0; });
    await page.waitForFunction(() => Number(document.querySelector('.admin-match-scroll').dataset.windowBase) === 0 && !document.querySelector('.admin-match-scroll [role=status]'));
    await region.focus();
    await page.keyboard.press('PageDown');
    await page.waitForFunction(() => document.querySelector('.admin-match-scroll').scrollTop > 0);
    const downTop = await region.evaluate(el => el.scrollTop);
    await page.keyboard.press('PageUp');
    await page.waitForFunction(top => document.querySelector('.admin-match-scroll').scrollTop < top, downTop);
    await region.evaluate(el => { el.scrollTop = 0; });
    await page.keyboard.press('ArrowDown');
    await page.waitForFunction(() => document.querySelector('.admin-match-scroll').scrollTop > 0);
    const arrowTop = await region.evaluate(el => el.scrollTop);
    await page.keyboard.press('ArrowUp');
    await page.waitForFunction(top => document.querySelector('.admin-match-scroll').scrollTop < top, arrowTop);
    checks.push('Delayed/failed evicted forward spacer jump preserves visible retained rows and constant scalar extent; retry refills; PageDown/PageUp/ArrowDown/ArrowUp scroll labelled region');
    await page.screenshot({ path: `bounded-${page.viewportSize().width}-populated-101.png`, fullPage: true });
    if (page.viewportSize().width === 390) {
      phase = 'native simulated touch';
      const cdp = await page.context().newCDPSession(page);
      await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 1 });
      const tap = async locator => {
        await locator.scrollIntoViewIfNeeded();
        const box = await locator.boundingBox();
        verify(box && box.width > 0 && box.height > 0, 'Native touch target has no box');
        const x = box.x + box.width / 2, y = box.y + box.height / 2;
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
      };
      await tap(page.locator('.admin-match-row').first().getByRole('button', { name: 'Match title', exact: true }));
      await page.getByRole('dialog').waitFor();
      await tap(page.getByRole('button', { name: 'Cancel', exact: true }));
      await page.getByRole('dialog').waitFor({ state: 'hidden' });
      await region.evaluate(el => { el.scrollTop = 0; el.addEventListener('touchstart', event => { window.__boundedTrustedTouch = event.isTrusted; }, { once: true }); });
      const box = await region.boundingBox();
      const x = box.x + box.width * .4;
      const startY = Math.min(page.viewportSize().height - 30, box.y + box.height - 35);
      const endY = Math.max(30, box.y + 35);
      verify(startY - endY > 100, 'Native touch swipe has insufficient visible region');
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: startY }] });
      for (let step = 1; step <= 12; step++) {
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: startY + (endY - startY) * step / 12 }] });
        await page.waitForTimeout(20);
      }
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
      await page.waitForFunction(() => document.querySelector('.admin-match-scroll').scrollTop > 0);
      metrics.simulatedTouchScroll = await region.evaluate(el => el.scrollTop);
      metrics.simulatedTouchTrusted = await page.evaluate(() => window.__boundedTrustedTouch === true);
      verify(metrics.simulatedTouchTrusted, 'CDP touch was not native trusted input');
      await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: false });
      await cdp.detach();
      await page.waitForFunction(() => {
        const top = document.querySelector('.admin-match-scroll').scrollTop, body = window.scrollY;
        const old = window.__boundedScrollStable;
        if (!old || old.top !== top || old.body !== body) window.__boundedScrollStable = { top, body, since: performance.now() };
        return performance.now() - window.__boundedScrollStable.since > 500;
      }, null, { timeout: 10000 });
      checks.push('Chrome native CDP simulated touch: trusted tap opens match, trusted Cancel tap dismisses, swipe scrolls region; not a physical phone');
    }
    phase = 'modal return';
    await region.evaluate((el, top) => { el.scrollTop = top; }, 8 * rowHeight);
    await idle();
    const id = expectedId(8);
    await page.waitForFunction(({ top, id }) => {
      const r = document.querySelector('.admin-match-scroll');
      const row = document.querySelector(`[data-match-id="${id}"]`);
      return r.scrollTop === top && row && row.getBoundingClientRect().top >= r.getBoundingClientRect().top;
    }, { top: 8 * rowHeight, id });
    const row = page.locator(`[data-match-id="${id}"]`);
    const button = row.getByRole('button', { name: 'Match title', exact: true });
    for (const close of ['Cancel', 'Escape', 'Back']) {
      const before = await region.evaluate(el => el.scrollTop);
      await button.click();
      await page.getByRole('dialog').waitFor();
      await page.getByLabel('Metadata ID', { exact: true }).fill('tt7654321');
      if (close === 'Cancel') await page.getByRole('button', { name: 'Cancel', exact: true }).click();
      else if (close === 'Escape') await page.keyboard.press('Escape');
      else await page.goBack();
      await page.getByRole('dialog').waitFor({ state: 'hidden' });
      await page.waitForFunction(({ id, before }) => {
        const r = document.querySelector('.admin-match-scroll');
        return Math.abs(r.scrollTop - before) < 2 && document.activeElement?.closest('[data-match-id]')?.dataset.matchId === id;
      }, { id, before }).catch(async error => {
        const observed = await page.evaluate(() => ({ top: document.querySelector('.admin-match-scroll').scrollTop,
          focusedId: document.activeElement?.closest('[data-match-id]')?.dataset.matchId, focusedTag: document.activeElement?.tagName }));
        throw new Error(close + ' restore expected=' + JSON.stringify({ id, before }) + '; observed=' + JSON.stringify(observed) + '; ' + error.message);
      });
    }
    checks.push('Cancel/Escape/browser Back close only dialog and restore exact scroll/opener focus');
    phase = 'failed then actual match save';
    await button.click();
    await page.getByLabel('Metadata ID', { exact: true }).fill('tt7654321');
    let failSave = true;
    const saveRoute = async route => {
      if (route.request().method() === 'PUT' && failSave) { failSave = false; await route.abort('failed'); }
      else await route.continue();
    };
    await page.route('**/api/v2/iptv/matches', saveRoute);
    await page.getByRole('button', { name: 'Save match', exact: true }).click();
    await page.getByRole('dialog').getByRole('alert').waitFor();
    verify(await page.getByLabel('Metadata ID', { exact: true }).inputValue() === 'tt7654321', 'Failed save lost edit');
    await page.unroute('**/api/v2/iptv/matches', saveRoute);
    const saved = page.waitForResponse(r => r.url() === cfg.origin + '/api/v2/iptv/matches' && r.request().method() === 'PUT');
    await page.getByRole('button', { name: 'Save match', exact: true }).click();
    verify((await saved).status() === 200, 'Actual saved match');
    await page.getByText('Metadata match saved', { exact: true }).waitFor();
    await page.locator(`[data-match-id="${id}"]`).getByRole('button', { name: 'Edit match', exact: true }).click();
    verify(await page.getByLabel('Metadata ID', { exact: true }).inputValue() === 'tt7654321', 'Edit saved match missing metadata');
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    checks.push('Transport save failure retains edits; actual200 retry/save updates only row and Edit match retains value');
    phase = 'actual revision stale recovery';
    const staleBefore = await measure();
    await region.evaluate((el, target) => { el.scrollTop = target; }, (staleBefore.base + staleBefore.rows - 5) * rowHeight);
    await page.getByRole('button', { name: 'Refresh titles', exact: true }).waitFor();
    const staleAfter = await measure();
    verify(staleAfter.rows === staleBefore.rows && staleAfter.base === staleBefore.base, 'Stale response dropped retained rows');
    const refreshed = page.waitForResponse(r => r.url().includes('/api/v2/iptv/matches?') && r.url().includes('provider_id=101') && !r.url().includes('cursor=') && r.ok());
    await page.getByRole('button', { name: 'Refresh titles', exact: true }).click();
    await refreshed;
    await idle();
    verify(await region.evaluate(el => el.scrollTop) === 0, 'Stale refresh did not reset position');
    checks.push('Actual saved-match revision invalidates cursor; safe Refresh titles preserves rows then resets');
    phase = 'direction retry';
    let failRead = true;
    const readRoute = async route => {
      if (route.request().url().includes('cursor=') && failRead) { failRead = false; await route.abort('failed'); }
      else await route.continue();
    };
    await page.route('**/api/v2/iptv/matches?*', readRoute);
    const beforeRetry = await measure();
    await region.evaluate((el, target) => { el.scrollTop = target; }, (beforeRetry.base + beforeRetry.rows - 5) * rowHeight);
    await page.getByRole('button', { name: 'Try again', exact: true }).waitFor();
    const failed = await measure();
    verify(failed.base === beforeRetry.base && failed.rows === beforeRetry.rows, 'Read failure dropped window');
    await page.unroute('**/api/v2/iptv/matches?*', readRoute);
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await page.waitForFunction(extent => Number(document.querySelector('.admin-match-scroll').dataset.visitedExtent) > extent, beforeRetry.extent);
    await idle();
    checks.push('Actual cursor-direction transport interruption preserves rows and position; Try again refills same direction');
    phase = 'filter cancellation';
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    let delayed = false;
    const delayRoute = async route => {
      if (route.request().url().includes('provider_id=102') && !delayed) { delayed = true; await gate; }
      try { await route.continue(); } catch { /* aborted by actual scoped client */ }
    };
    await page.route('**/api/v2/iptv/matches?*', delayRoute);
    await page.locator('select').first().selectOption('102');
    await page.waitForTimeout(100);
    await page.locator('select').first().selectOption('605');
    await page.getByText('Tail provider title', { exact: true }).waitFor();
    release();
    await page.waitForTimeout(200);
    verify(await region.evaluate(el => el.scrollTop) === 0 && await page.locator('.admin-match-row').count() === 1, 'Late filter response polluted result');
    await page.unroute('**/api/v2/iptv/matches?*', delayRoute);
    await page.locator('select').nth(1).selectOption('series');
    await page.getByRole('heading', { name: 'No matching titles', exact: true }).waitFor();
    await page.locator('select').nth(1).selectOption('movie');
    await page.getByText('Tail provider title', { exact: true }).waitFor();
    await page.getByLabel('Search titles', { exact: true }).fill('does-not-exist');
    await page.getByRole('heading', { name: 'No matching titles', exact: true }).waitFor();
    await page.getByLabel('Search titles', { exact: true }).fill('Tail provider');
    await page.getByText('Tail provider title', { exact: true }).waitFor();
    checks.push('Delayed real provider read canceled; raw605 wins; type and250ms search filters reset correctly');
    await page.screenshot({ path: `bounded-${page.viewportSize().width}-list.png`, fullPage: true });
    await page.getByRole('button', { name: 'Match title', exact: true }).click();
    await page.screenshot({ path: `bounded-${page.viewportSize().width}-dialog.png`, fullPage: true });
    await page.keyboard.press('Escape');
    verify(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Horizontal overflow');
    phase = 'account transition';
    let releaseAccountRead;
    const accountGate = new Promise(resolve => { releaseAccountRead = resolve; });
    let accountReadPending = false;
    const accountRoute = async route => {
      if (route.request().url().includes('provider_id=102')) { accountReadPending = true; await accountGate; }
      try { await route.continue(); } catch { /* actual account scope canceled */ }
    };
    await page.route('**/api/v2/iptv/matches?*', accountRoute);
    await page.getByLabel('Search titles', { exact: true }).fill('');
    await page.locator('select').first().selectOption('102');
    await page.waitForTimeout(350);
    verify(accountReadPending, 'Actual read was not pending at signout');
    const navOpener = page.getByRole('button', { name: 'Open navigation', exact: true });
    if (await navOpener.isVisible()) await navOpener.click();
    await page.getByRole('button', { name: 'Sign out', exact: true }).last().click();
    await page.getByRole('dialog', { name: 'Sign out?' }).getByRole('button', { name: 'Sign out', exact: true }).click();
    await page.getByRole('heading', { name: 'Sign in', exact: true }).waitFor();
    releaseAccountRead();
    await page.unroute('**/api/v2/iptv/matches?*', accountRoute);
    await login('qa_foreign', 3);
    await navigate('VOD matches');
    await idle();
    verify(await page.evaluate(() => Array.from(document.querySelector('select').options, o => o.value).includes('201')), 'Foreign account owned provider missing');
    verify(await page.locator('[data-match-id^="vod:101:"]').count() === 0 && await page.getByRole('dialog').count() === 0, 'Prior account rows/dialog leaked');
    checks.push('Actual UI signout during pending real read cancels prior scope; foreign-account transition clears provider/row/dialog state');
    return 'BOUNDED_PASS ' + JSON.stringify({ viewport: page.viewportSize(), metrics, checks, saved_id: id,
      boundary: 'real trusted HTTPS/API, synthetic sealed catalog, transport fault injection only; browser viewports not physical phones or deployment' });
  } catch (error) {
    await page.screenshot({ path: `bounded-${page.viewportSize().width}-failure.png`, fullPage: true });
    throw new Error('Bounded acceptance phase ' + phase + ': ' + error.message);
  }
}
