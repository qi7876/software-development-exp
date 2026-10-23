import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import {
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const project = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const secret = "browser-test-key";

async function tree(root) {
  const result = {};
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    result[entry.name] = entry.isDirectory()
      ? await tree(path)
      : (await readFile(path)).toString("hex");
  }
  return result;
}

async function startServer(t, apiSecret = secret, webUi = true) {
  const directory = await mkdtemp(join(tmpdir(), "bak-browser-"));
  let child;
  t.after(async () => {
    if (child && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit");
      child.kill("SIGTERM");
      const force = setTimeout(() => child.kill("SIGKILL"), 5000);
      await exited;
      clearTimeout(force);
    }
    await rm(directory, { recursive: true, force: true });
  });
  for (const path of [
    "source/空目录",
    "repo with space",
    "restored",
    "occupied",
  ]) {
    await mkdir(join(directory, path), { recursive: true });
  }
  await writeFile(
    join(directory, "source/你好.txt"),
    "Round trip · 备份与恢复\n",
  );
  await writeFile(join(directory, "occupied/keep.txt"), "keep me");
  await writeFile(
    join(directory, "config.json"),
    JSON.stringify({
      listen: "127.0.0.1:0",
      secret_key: apiSecret,
      web_ui: { enabled: webUi },
    }),
  );
  child = spawn(join(project, "target/debug/bak"), ["-d", directory], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const url = await new Promise((resolveReady, reject) => {
    let output = "";
    let errors = "";
    const timer = setTimeout(
      () => reject(new Error(`Server did not start: ${output}\n${errors}`)),
      10000,
    );
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.stderr.on("data", (data) => {
      errors += data;
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`Server exited (${code}): ${errors}`));
    });
    child.stdout.on("data", (data) => {
      output += data;
      const match = output.match(/Listening on ([^\r\n]+)/);
      if (match) {
        clearTimeout(timer);
        resolveReady(`http://${match[1]}`);
      }
    });
  });
  return { directory, url };
}

test(
  "console backs up, restores, reports failures and retains history",
  { timeout: 90000 },
  async (t) => {
    const { directory, url } = await startServer(t);
    let browser;
    t.after(async () => {
      await browser?.close();
    });
    browser = await chromium.launch({
      channel: process.env.BAK_BROWSER_CHANNEL || undefined,
    });
    const page = await browser.newPage({
      viewport: { width: 1440, height: 1000 },
      reducedMotion: "reduce",
    });
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (entry) => {
      if (
        entry.type() === "error" &&
        /Content Security Policy|Refused to/.test(entry.text())
      )
        errors.push(entry.text());
    });
    await page.goto(url);
    assert.equal(await page.locator("main > .view:visible").count(), 1);
    assert.equal(
      await page.locator(".topbar, .page-heading, .stats").count(),
      0,
    );
    assert.equal(await page.locator("#source").isDisabled(), true);
    await page.locator("#open-connection").click();
    await page.locator("#secret").fill("wrong-key");
    await page.locator("#connect-button").click();
    await page.locator("#connection-message.error").waitFor();
    assert.equal(await page.locator("#source").isDisabled(), true);

    async function connect() {
      await page.locator("#open-connection").click();
      if (await page.locator(".connection-row").count()) {
        await page
          .locator(".connection-row")
          .getByRole("button", { name: "连接", exact: true })
          .click();
      } else {
        await page.locator("#secret").fill(secret);
        await page.locator("#connect-button").click();
      }
      await page.waitForFunction(
        () =>
          document.getElementById("connection-state").textContent === "已连接",
      );
      await page.waitForFunction(
        () => !document.getElementById("refresh-jobs").disabled,
      );
    }
    async function view(name) {
      await page.locator(`nav a[href="#${name}"]`).click();
      await page.locator(`#${name}`).waitFor();
      assert.equal(await page.locator("main > .view:visible").count(), 1);
      assert.equal(
        await page
          .locator(`nav a[href="#${name}"]`)
          .getAttribute("aria-current"),
        "page",
      );
    }
    await connect();
    await view("backup");
    await page.locator("#source").fill("source");
    await page.locator("#backup-repository").fill("repo with space");
    await view("repository");
    await page.goBack();
    await page.locator("#backup").waitFor();
    assert.equal(await page.locator("#source").inputValue(), "source");
    assert.equal(
      await page.locator("#backup-repository").inputValue(),
      "repo with space",
    );
    await page.locator("#backup-button").click();
    await page.locator("#backup-job-link").click();
    await page.locator(".job .badge.succeeded").waitFor();
    await page.getByRole("button", { name: "查看备份", exact: true }).click();
    await page.locator(".backup-row button").waitFor();
    assert.equal(
      await page.locator("#repository-label").textContent(),
      "1 份备份",
    );

    await view("repository");
    await page.locator(".backup-row button").click();
    await page.locator("#destination").fill("occupied");
    await page.locator("#restore-button").click();
    await page.locator(".job .badge.failed").waitFor();
    assert.equal(
      await readFile(join(directory, "occupied/keep.txt"), "utf8"),
      "keep me",
    );

    await view("repository");
    await page.locator(".backup-row button").click();
    await page.locator("#destination").fill("restored");
    await page.locator("#restore-button").click();
    await page.waitForFunction(
      () => document.querySelectorAll(".job .badge.succeeded").length === 2,
    );
    assert.deepEqual(
      await tree(join(directory, "restored")),
      await tree(join(directory, "source")),
    );
    assert.equal(await page.locator("#jobs").isVisible(), true);
    await page.locator(".job summary").first().click();
    await page.locator("#refresh-jobs").click();
    await page.waitForFunction(
      () => !document.getElementById("refresh-jobs").disabled,
    );
    assert.equal(
      await page.locator(".job details").first().getAttribute("open"),
      "",
    );

    if (process.env.BAK_UI_SCREENSHOT) {
      await page.evaluate(() => scrollTo({ top: 0, behavior: "instant" }));
      await page.screenshot({
        path: process.env.BAK_UI_SCREENSHOT,
        fullPage: true,
      });
    }
    // Refresh restores the saved endpoint/key and verifies it before reloading history.
    await page.reload();
    assert.equal(await page.locator("#secret").inputValue(), "");
    assert.equal(
      await page.evaluate(
        () =>
          JSON.parse(localStorage.getItem("bak.connections.v1")).connections[0]
            .key,
      ),
      secret,
    );
    await page.waitForFunction(
      () =>
        document.getElementById("connection-state").textContent === "已连接",
    );
    await page.waitForFunction(
      () => document.querySelectorAll(".job").length === 3,
    );

    // Render server paths/errors as text, and exercise real cursor pagination.
    for (let index = 0; index < 19; index++) {
      const response = await fetch(`${url}/api/backups`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${secret}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          source: `missing-<img src=x onerror=alert(1)>-${index}`,
          repository: "repo with space",
        }),
      });
      assert.equal(response.status, 202);
    }
    await page.locator("#refresh-jobs").click();
    await page.waitForFunction(
      () =>
        document.querySelectorAll(".job").length === 20 &&
        !document.getElementById("next-jobs").disabled,
    );
    assert.equal(await page.locator("#job-list img").count(), 0);
    const firstIds = await page.locator(".job-id").allTextContents();
    await page.locator("#next-jobs").click();
    await page.waitForFunction(
      () => document.getElementById("jobs-page").textContent === "第 2 页",
    );
    assert.equal(await page.locator(".job").count(), 2);
    assert.equal(await page.locator("#next-jobs").isDisabled(), true);
    for (const id of await page.locator(".job-id").allTextContents())
      assert.equal(firstIds.includes(id), false);
    await page.locator("#previous-jobs").click();
    await page.waitForFunction(
      () => document.getElementById("jobs-page").textContent === "第 1 页",
    );

    await page.route("**/api/jobs?*", (route) => route.abort());
    await page.locator("#refresh-jobs").click();
    await page.locator("#jobs-message.error").waitFor();
    assert.equal(await page.locator(".job").count(), 20);
    await page.unroute("**/api/jobs?*");
    await page.locator("#refresh-jobs").click();
    await page.waitForFunction(
      () => document.getElementById("jobs-message").textContent === "",
    );

    await view("repository");
    await page.locator("#repository-path").fill("missing-repository");
    await page.locator("#repository-button").click();
    await page.locator("#repository-message.error").waitFor();
    assert.equal(await page.locator(".backup-row").count(), 0);

    await view("backup");
    await view("repository");
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
      true,
    );
    await page.locator("#disconnect-button").click();
    assert.equal(await page.locator(".job").count(), 0);
    assert.equal(await page.locator("#source").isDisabled(), true);
    await connect();
    await view("jobs");
    await page.waitForFunction(
      () => document.querySelectorAll(".job").length === 20,
    );
    assert.deepEqual(errors, []);
  },
);

test(
  "saved connections switch real API origins and survive closing the page",
  { timeout: 90000 },
  async (t) => {
    const local = await startServer(t);
    const remote = await startServer(t, "remote-api-key", false);
    const browser = await chromium.launch({
      channel: process.env.BAK_BROWSER_CHANNEL || undefined,
    });
    t.after(() => browser.close());
    const context = await browser.newContext({
      viewport: { width: 1440, height: 1000 },
    });
    let page = await context.newPage();
    const errors = [];
    context.on("page", (opened) =>
      opened.on("pageerror", (error) => errors.push(error.message)),
    );
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(local.url);

    async function manager() {
      await page.locator("#open-connection").click();
      await page.locator("#connections").waitFor();
    }
    async function connected(url) {
      await page.waitForFunction(
        (target) =>
          document.getElementById("connection-host").textContent === target &&
          document.getElementById("connection-state").textContent === "已连接",
        url,
      );
      await page.waitForFunction(
        () =>
          !document.getElementById("connect-button").disabled &&
          !document.getElementById("refresh-jobs").disabled,
      );
    }
    async function save(name, url, key) {
      await page.locator("#profile-name").fill(name);
      await page.locator("#api-url").fill(url);
      await page.locator("#secret").fill(key);
      await page.locator("#connect-button").click();
    }
    async function switchTo(name, url) {
      await manager();
      await page
        .locator(".connection-row")
        .filter({ hasText: name })
        .getByRole("button", { name: "连接", exact: true })
        .click();
      await connected(url);
    }
    async function stored() {
      return page.evaluate(() =>
        JSON.parse(localStorage.getItem("bak.connections.v1")),
      );
    }

    await manager();
    await save("本机", local.url.replace("http://", ""), secret);
    await connected(local.url);
    await page.locator('nav a[href="#backup"]').click();
    await page.locator("#source").fill("/local-only/source");
    await page.locator("#backup-repository").fill("/local-only/repository");
    await manager();
    await save("远程", remote.url, "wrong-key");
    await page.locator("#connection-message.error").waitFor();
    assert.equal(
      await page.locator("#connection-host").textContent(),
      local.url,
    );
    assert.equal(
      await page.locator("#source").inputValue(),
      "/local-only/source",
    );
    assert.equal((await stored()).connections.length, 1);

    await save("远程", remote.url, "remote-api-key");
    await connected(remote.url);
    assert.equal(await page.locator("#source").inputValue(), "");
    assert.equal(await page.locator("#backup-repository").inputValue(), "");
    assert.equal((await stored()).connections.length, 2);
    assert.equal(
      (await page.locator("#connection-list").textContent()).includes(
        "remote-api-key",
      ),
      false,
    );

    // The UI is served by the first process, while all operation requests go to the second.
    await page.locator('nav a[href="#backup"]').click();
    await page.locator("#source").fill("source");
    await page.locator("#backup-repository").fill("repo with space");
    await page.locator("#backup-button").click();
    await page.locator("#backup-job-link").click();
    await page.locator(".job .badge.succeeded").waitFor();
    const localHistory = await fetch(`${local.url}/api/jobs`, {
      headers: { Authorization: `Bearer ${secret}` },
    });
    assert.deepEqual((await localHistory.json()).jobs, []);
    const remoteHistory = await fetch(`${remote.url}/api/jobs`, {
      headers: { Authorization: "Bearer remote-api-key" },
    });
    assert.equal((await remoteHistory.json()).jobs.length, 1);

    await switchTo("本机", local.url);
    assert.equal(await page.locator(".job").count(), 0);
    assert.equal(
      await page.locator("#repository-label").textContent(),
      "尚未载入仓库",
    );
    await switchTo("远程", remote.url);
    await page.waitForFunction(
      () => document.querySelectorAll(".job").length === 1,
    );
    await page
      .locator(".connection-row")
      .filter({ hasText: "远程" })
      .getByRole("button", { name: "编辑", exact: true })
      .click();
    assert.equal(await page.locator("#secret").inputValue(), "remote-api-key");
    await save("远程备份", remote.url, "wrong-key");
    await page.locator("#connection-message.error").waitFor();
    assert.equal(
      (await stored()).connections.find((item) => item.url === remote.url).key,
      "remote-api-key",
    );
    await save("远程备份", remote.url, "remote-api-key");
    await connected(remote.url);
    assert.equal((await stored()).connections.length, 2);

    await page.close();
    page = await context.newPage();
    await page.goto(local.url);
    await connected(remote.url);
    await page.waitForFunction(
      () => document.querySelectorAll(".job").length === 1,
    );
    await page.locator("#disconnect-button").click();
    assert.equal((await stored()).selectedUrl, null);
    assert.equal((await stored()).connections.length, 2);
    await page.reload();
    assert.equal(
      await page.locator("#connection-state").textContent(),
      "未连接",
    );
    await switchTo("远程备份", remote.url);

    await page
      .locator(".connection-row")
      .filter({ hasText: "本机" })
      .getByRole("button", { name: "删除", exact: true })
      .click();
    assert.equal(
      await page.locator("#connection-host").textContent(),
      remote.url,
    );
    await page
      .locator(".connection-row")
      .filter({ hasText: "远程备份" })
      .getByRole("button", { name: "删除", exact: true })
      .click();
    assert.deepEqual(await stored(), { connections: [], selectedUrl: null });
    assert.equal(
      await page.locator("#connection-state").textContent(),
      "未连接",
    );
    assert.equal(await page.locator(".job").count(), 0);
    await page.reload();
    assert.equal(
      await page.locator("#connection-state").textContent(),
      "未连接",
    );
    assert.deepEqual(errors, []);
  },
);
