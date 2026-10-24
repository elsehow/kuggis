// Browser origin/CSP tests with a local authenticated-gateway fixture. This is
// not a claim that production DNS, TLS, or sign-in has been exercised.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const { chromium } = await import(pathToFileURL(process.env.KUGGIS_TEST_PLAYWRIGHT).href);
const browser = await chromium.launch({headless: true, executablePath: process.env.KUGGIS_TEST_BROWSER});
const [address, first, second] = process.argv.slice(2);
try {
  const context = await browser.newContext();
  let authenticated = true;
  await context.route(url => [new URL(first).origin, new URL(second).origin].includes(url.origin), async route => {
    const request = route.request(), url = new URL(request.url());
    const original = await request.allHeaders();
    const headers = {'host': url.host, 'x-forwarded-host': url.host, 'x-forwarded-proto': 'https'};
    for (const key of ['origin', 'sec-fetch-site', 'sec-fetch-mode', 'sec-fetch-dest']) if (original[key]) headers[key] = original[key];
    if (authenticated) headers['x-exedev-userid'] = 'reader';
    const response = await context.request.get(`http://${address}${url.pathname}`, {headers, maxRedirects: 0});
    await route.fulfill({response});
  });
  const page = await context.newPage();
  assert.equal((await page.goto(first)).status(), 200);
  await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'Modules and private fetch work');
  assert.equal(await page.evaluate(async url => {
    try { await fetch(url).then(r => r.text()); return 'read'; } catch { return 'blocked'; }
  }, second), 'blocked');
  await page.evaluate(() => localStorage.setItem('private-test', 'only-first-site'));
  const other = await context.newPage();
  await other.goto(second);
  await other.waitForFunction(() => document.querySelector('h1')?.textContent === 'Modules and private fetch work');
  assert.equal(await other.evaluate(() => localStorage.getItem('private-test')), null);
  assert.equal(await page.evaluate(async () => {
    try { await navigator.serviceWorker.register('/main.js'); return 'registered'; } catch { return 'blocked'; }
  }), 'blocked');
  authenticated = false;
  assert.equal((await page.goto(first)).status(), 403);
  assert.equal((await page.content()).includes('Modules and private fetch work'), false);
  console.log('Private sites: modules, JSON fetch, origin isolation, service-worker denial, and anonymous denial passed.');
} finally { await browser.close(); }
