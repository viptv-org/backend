async page => {
  const cfg = __FIXTURE_CONFIG__;
  const origin = cfg.origin;
  const checks = [], gaps = [];
  let phase = 'real login';
  try {
  const verify = (condition, label) => { if (!condition) throw new Error(label); };
  const waitResponse = predicate => page.waitForResponse(predicate).catch(error => { throw new Error('Response phase: '+phase+'; '+error.message); });
  const api = async (context, path, method = 'GET', body) => {
    verify(path.startsWith('/api/'), 'Nonfixture API path');
    return page.evaluate(async ({ path, method, body }) => {
      const csrf = (await (await fetch('/api/auth/status', { credentials: 'same-origin' })).json()).csrf_token;
      const response = await fetch(path, { method, credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', ...(csrf ? { 'x-csrf-token': csrf } : {}) },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      return { status: response.status, body: await response.json() };
    }, { path, method, body });
  };
  const login = async (username, profile) => {
    await page.context().clearCookies();
    const result = await api(page.context(), '/api/auth/login', 'POST', { username, password: cfg.password });
    verify(result.status === 200, 'Real fixture login failed');
    verify((await api(page.context(), '/api/auth/profile', 'POST', { profile_id: profile })).status === 200, 'Real profile selection failed');
    await page.goto(origin);
    await page.getByRole('heading', { name: 'Account', exact: true }).waitFor();
  };
  await page.route('**/*', route => {
    const url = route.request().url();
    return url === origin || url.startsWith(origin + '/') ? route.continue() : route.abort(); // Never intercept a local API response.
  });
  const navigate = async name => {
    const opener = page.getByRole('button', { name: 'Open navigation', exact: true });
    if (await opener.isVisible()) await opener.click();
    await page.getByRole('navigation', { name: 'Main navigation' }).getByRole('button', { name, exact: true }).click();
  };
  await login('qa_member', 2);
  if (cfg.extra_providers) {
    const first = await api(page.context(), '/api/v2/iptv/connections?limit=200');
    verify(first.status === 200 && first.body.items.length === 200 && first.body.next_cursor, 'Actual >200 provider fixture missing');
    const tail = await api(page.context(), '/api/v2/iptv/matches?provider_id=605&limit=50');
    verify(tail.status === 200 && tail.body.items.some(item => item.vod_id === 'vod:605:tail'), 'Actual tail provider is not account-accessible');
    await navigate('VOD matches');
    await page.locator('select').first().waitFor();
    await page.waitForFunction(() => document.querySelector('select')?.options.length >= 200);
    const dropdown = page.locator('select').first();
    const values = await dropdown.locator('option').evaluateAll(options => options.map(option => option.value));
    verify(!values.includes('605'), 'Known >200 filter gap unexpectedly resolved; inspect test expectation');
    return 'POPULATED_PASS ' + JSON.stringify({ viewport: page.viewportSize(), checks: ['actual208 owned providers; tail605 API accessible'], gaps: [{kind:'preexisting_provider_filter_first200_only', visibleProviderOptions:values.length-1, inaccessibleOwnedProvider:605}] });
  }
  verify(await page.getByRole('button', { name: 'Accounts', exact: true }).count() === 0, 'Member saw operator navigation');
  const providers = await api(page.context(), '/api/v2/iptv/connections');
  verify(providers.status === 200 && providers.body.items.length === 3, 'Owned three-provider fixture missing');
  verify(providers.body.items.every(p => [101, 102, 103].includes(Number(p.id)) && p.credentials_encrypted), 'Foreign/plaintext connection exposed');
  const serialized = JSON.stringify(providers.body);
  verify(!serialized.includes('fixture.invalid') && !serialized.includes('synthetic-password'), 'Connection list exposed credentials');
  await navigate('Xtream connections');
  phase = 'UI default save';
  await page.getByRole('heading', { name: 'Synthetic provider 101', exact: true }).waitFor();
  const targetDefault = (await api(page.context(), '/api/v2/iptv/live-default')).body.catalog_id === 103 ? 102 : 103;
  await page.getByRole('button', { name: `Use Synthetic provider ${targetDefault} as default live playlist`, exact: true }).click();
  const savedDefault = waitResponse(r => r.url() === origin + '/api/v2/iptv/live-default' && r.request().method() === 'PUT');
  await page.getByRole('button', { name: 'Use this playlist', exact: true }).click();
  verify((await savedDefault).status() === 200, 'UI default save failed');
  verify(Number((await api(page.context(), '/api/v2/iptv/live-default')).body.catalog_id) === targetDefault, 'Default did not persist');
  await page.getByRole('button', { name: 'Edit Synthetic provider 102', exact: true }).click();
  phase = 'UI scope save';
  await page.getByLabel('Connection name', { exact: true }).fill('Synthetic provider 102');
  verify(await page.locator('input[type=password]').count() === 0, 'Stored secret appeared in edit form');
  const series = page.getByLabel('Series', { exact: true });
  const expectedSeries = !(await series.isChecked());
  await series.setChecked(expectedSeries);
  const saveScope = waitResponse(r => r.url() === origin + '/api/v2/iptv/connections/102' && r.request().method() === 'PATCH');
  await page.getByRole('button', { name: 'Save connection', exact: true }).click();
  verify((await saveScope).status() === 200, 'Owned UI connection save failed');
  verify((await api(page.context(), '/api/v2/iptv/connections')).body.items.find(item => Number(item.id) === 102).enable_series === expectedSeries, 'UI scope save did not persist');
  await navigate('Add-ons');
  await page.getByRole('heading', { name: 'Synthetic addon 2', exact: true }).waitFor();
  verify(!((await api(page.context(), '/api/v2/addons')).body.items.some(item => item.manifest_url)), 'Manifest URL exposed');
  checks.push('real owned connection/default save and encrypted addon metadata');

  await navigate('VOD matches');
  phase = 'provider filter response';
  const region = page.getByRole('region', { name: 'Unmatched provider titles' });
  await page.locator('.admin-match-row').first().waitFor();
  const filtered = waitResponse(r => r.url().includes('/api/v2/iptv/matches?') && /[?&]provider_id=101(?:&|$)/.test(r.url()) && r.ok());
  await page.locator('select').first().selectOption('101');
  await filtered;
  await page.locator('.admin-match-row').first().waitFor();
  const startHeight = await region.evaluate(el => el.scrollHeight);
  let responses = 0, maximumPayload = 0;
  const onResponse = async response => {
    if (response.url().includes('/api/v2/iptv/matches?') && response.ok()) {
      const body = await response.json(); responses++; maximumPayload = Math.max(maximumPayload, body.items.length);
      verify(!('total' in body), 'Exact total appeared in lazy API');
    }
  };
  page.on('response', onResponse);
  try {
    for (let index = 0; index < 20; index++) {
      phase = 'cursor traversal page ' + index;
      const previous = await region.evaluate(el => el.scrollHeight);
      const next = waitResponse(r => r.url().includes('/api/v2/iptv/matches?') && r.ok());
      await region.evaluate(el => { el.scrollTop = el.scrollHeight; });
      await next;
      await page.waitForFunction(height => document.querySelector('.admin-match-scroll').scrollHeight > height, previous);
      verify(await page.locator('.admin-match-row').count() <= 30, 'Unbounded VOD DOM');
    }
    const height = await region.evaluate(el => el.scrollHeight);
    verify(responses >= 20 && maximumPayload <= 50, 'Lazy page payload/count evidence missing');
    const rowHeight = page.viewportSize().width < 768 ? 184 : 112;
    gaps.push({ kind: 'preexisting_retained_model_growth', observedVirtualRows: Math.round(height / rowHeight), initialVirtualRows: Math.round(startHeight / rowHeight), boundedDom: await page.locator('.admin-match-row').count(), traversedPages: responses });
  } finally { page.off('response', onResponse); }
  const chosenRow = page.locator('.admin-match-row').first();
  phase = 'actual selected-row match save';
  const chosenId = await chosenRow.getAttribute('data-match-id');
  await chosenRow.getByRole('button', { name: 'Match title', exact: true }).click();
  await page.getByLabel('Metadata ID', { exact: true }).fill('tt7654321');
  const matchSave = waitResponse(r => r.url() === origin + '/api/v2/iptv/matches' && r.request().method() === 'PUT');
  await page.getByRole('button', { name: 'Save match', exact: true }).click();
  verify((await matchSave).status() === 200, 'Actual owned metadata save failed');
  await page.getByText('Metadata match saved', { exact: true }).waitFor();
  verify(chosenId.startsWith('vod:101:'), 'Foreign selected source mapping');
  checks.push('100k actual SQL catalog: twenty cursor pages, <=50 payloads, <=30 DOM rows, owned match save');

  await login('qa_foreign', 3);
  phase = 'foreign-account isolation';
  const denied = await api(page.context(), '/api/v2/iptv/connections/101', 'PATCH', { name: 'Forbidden' });
  verify(denied.status === 404, 'Cross-account connection write did not hide existence');
  const foreign = await api(page.context(), '/api/v2/iptv/connections');
  verify(foreign.body.items.length === 1 && Number(foreign.body.items[0].id) === 201, 'Cross-account list leaked sources');
  checks.push('actual foreign-account list and mutation isolation');

  await login('qa_operator', 1);
  phase = 'actual operator accounts and grants';
  await navigate('Accounts');
  await page.getByText('qa_member', { exact: true }).first().waitFor();
  const existing = await api(page.context(), '/api/v2/gateways');
  let gateway = existing.body.items.find(item => item.name === 'Synthetic operator gateway');
  verify(gateway, 'Offline unverified gateway fixture missing');
  verify((await api(page.context(), `/api/v2/gateways/${gateway.id}/grants`, 'PUT', { account_id: 2, enabled: true })).status === 200, 'Owner grant save failed');
  await navigate('Gateway grants');
  await page.getByRole('heading', { name: 'Synthetic operator gateway', exact: true }).waitFor();
  const grants = await api(page.context(), `/api/v2/gateways/${gateway.id}/grants`);
  verify(grants.status === 200 && grants.body.items.some(item => Number(item.account_id) === 2 && item.enabled), 'Actual recipient grant missing');
  verify(!JSON.stringify((await api(page.context(), '/api/v2/gateways')).body).includes('pgk_'), 'Gateway secret leaked');
  const checked = await api(page.context(), `/api/v2/gateways/${gateway.id}/check`, 'POST', {});
  verify(checked.status >= 400 && checked.body.error_code && !JSON.stringify(checked.body).includes('pgk_'), 'Unverified peer did not fail with safe real classification');
  verify(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Horizontal overflow');
  checks.push('actual operator accounts, offline unverified encrypted gateway read/grant and redaction; no verified registration/capacity claim');
  return 'POPULATED_PASS ' + JSON.stringify({ viewport: page.viewportSize(), checks, gaps,
    refusals: [{ kind: 'unverified_gateway_peer', status: checked.status, code: checked.body.error_code }],
    boundary: 'actual API/image; synthetic data; no provider/gateway media or deployment qualification' });
  } catch (error) {
    await page.screenshot({path:'populated-failure.png',fullPage:true});
    const body = await page.locator('body').innerText();
    throw new Error('Populated phase: '+phase+'; '+error.message+'; visible fixture state: '+body.slice(0,1500));
  }
}
