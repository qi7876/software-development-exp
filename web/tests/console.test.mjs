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

test(
  "console backs up, restores, reports failures and retains history",
  { timeout: 90000 },
  async (t) => {
    const directory = await mkdtemp(join(tmpdir(), "bak-browser-"));
    let child;
    let browser;
    t.after(async () => {
      await browser?.close();
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
        secret_key: secret,
        web_ui: { enabled: true },
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
      if (!(await page.locator("#connection-dialog").isVisible()))
        await page.locator("#open-connection").click();
      await page.locator("#secret").fill(secret);
      await page.locator("#connect-button").click();
      await page.waitForFunction(
        () =>
          document.getElementById("connection-state").textContent === "已连接",
      );
      await page.waitForFunction(
        () => !document.getElementById("refresh-jobs").disabled,
      );
      assert.equal(await page.locator("#connection-dialog").isVisible(), false);
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
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
      true,
    );
    if (process.env.BAK_UI_MOBILE_SCREENSHOT) {
      await page.evaluate(() => scrollTo({ top: 0, behavior: "instant" }));
      await page.screenshot({
        path: process.env.BAK_UI_MOBILE_SCREENSHOT,
        fullPage: false,
      });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Refresh keeps the key out of browser storage; reconnect reloads durable history.
    await page.reload();
    assert.equal(await page.locator("#secret").inputValue(), "");
    assert.equal(
      await page.evaluate(() => localStorage.length + sessionStorage.length),
      0,
    );
    await connect();
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

    await page.setViewportSize({ width: 390, height: 844 });
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
