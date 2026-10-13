#!/usr/bin/env node
/**
 * 一键发版：递增版本号（五处对齐）→ 提交 → 打附注 tag（正文为变更清单）→ 推送触发 CI 构建。
 *
 * 用法：
 *   node scripts/release.mjs            # patch +1
 *   node scripts/release.mjs 0.2.0      # 指定版本
 *   node scripts/release.mjs --dry-run  # 只打印将要做的改动
 *
 * 为什么用 Node 而不是 shell/PowerShell：
 *   - 版本号散落在 5 个文件、其中 package-lock.json 有 2 处，正则替换在 Node 里可控且可测；
 *   - 避免不同平台换行符（CRLF/LF）导致的匹配漂移；
 *   - 发版必须一次性改全，漏一处就会出现「应用内版本号显示错」或 `--locked` 构建失败。
 *
 * 为什么是**附注** tag 而不是轻量 tag：tag 正文是发布说明的唯一来源，
 * 而发布说明既进 GitHub Release 页面，也进 `latest.json` 的 `notes`
 * （应用内「应用更新」分区靠它渲染变更内容）。轻量 tag 没有正文，
 * 会让升级的用户看到一片空白。
 *
 * tag 正文的取法：**优先用 `release-notes/<tag>.md`**（人写的、面向用户的文案，
 * 随本次发版一起提交），没有才退化成「上一个 tag 以来的提交标题清单」。
 * 提交标题是给开发者看的（`fix(ci): …` 这种不该出现在升级提示里），
 * 所以人工文案优先；但宁可给一份朴素的清单，也不要给一片空白。
 */
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const TAURI_CONF = 'src-tauri/tauri.conf.json';
const CARGO_TOML = 'src-tauri/Cargo.toml';
const CARGO_LOCK = 'src-tauri/Cargo.lock';
const PACKAGE_JSON = 'package.json';
const PACKAGE_LOCK = 'package-lock.json';
const CRATE_NAME = 'kiro2api';

const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const explicit = args.find((a) => /^\d+\.\d+\.\d+$/.test(a)) ?? null;

if (args.some((a) => a.startsWith('-') && a !== '--dry-run')) {
  console.error('用法: node scripts/release.mjs [x.y.z] [--dry-run]');
  process.exit(1);
}

const abs = (rel) => join(ROOT, rel);
const read = (rel) => readFileSync(abs(rel), 'utf8');
const write = (rel, text) => writeFileSync(abs(rel), text, 'utf8');

function currentVersion() {
  const text = read(TAURI_CONF);
  const match = text.match(/"version"\s*:\s*"([^"]*)"/);
  if (!match) throw new Error(`无法从 ${TAURI_CONF} 读取版本号`);
  return match[1];
}

function bumpPatch(version) {
  const parts = version.split('.').map(Number);
  if (parts.length !== 3 || parts.some(Number.isNaN)) {
    throw new Error(`版本号格式不支持: ${version}`);
  }
  parts[2] += 1;
  return parts.join('.');
}

/** 只替换第一个匹配，避免误伤文件里其它同名字段。 */
function replaceOnce(text, pattern, replacement, label) {
  if (!pattern.test(text)) throw new Error(`未匹配到 ${label}`);
  return text.replace(pattern, replacement);
}

/**
 * 在内存里算出每个文件的新内容。返回 `{ rel, text }[]`（只含真正变化的文件）。
 *
 * 拆成「先算后写」是为了让 `--dry-run` 能真正跑一遍正则：只打印不改盘，
 * 但仍能暴露「某个文件的版本字段匹配不到」这类会在真实发版时才炸的问题。
 */
function computeVersionEdits(next) {
  const edits = [];

  const tauri = read(TAURI_CONF);
  const tauriNext = replaceOnce(
    tauri,
    /("version"\s*:\s*")[^"]*(")/,
    `$1${next}$2`,
    `${TAURI_CONF} 的 version`,
  );
  if (tauriNext !== tauri) edits.push({ rel: TAURI_CONF, text: tauriNext });

  const pkgRaw = read(PACKAGE_JSON);
  const pkg = JSON.parse(pkgRaw);
  if (pkg.version !== next) {
    pkg.version = next;
    edits.push({ rel: PACKAGE_JSON, text: `${JSON.stringify(pkg, null, 2)}\n` });
  }

  // package-lock.json 有 2 处版本号：顶层与 packages[""]。改文本以保留原有格式。
  if (existsSync(abs(PACKAGE_LOCK))) {
    const lock = read(PACKAGE_LOCK);
    const parsed = JSON.parse(lock);
    if (parsed.version !== next || parsed.packages?.['']?.version !== next) {
      let nextLock = replaceOnce(
        lock,
        /("version"\s*:\s*")[^"]*(")/,
        `$1${next}$2`,
        `${PACKAGE_LOCK} 顶层 version`,
      );
      nextLock = replaceOnce(
        nextLock,
        /("":\s*\{\s*"name":\s*"[^"]*",\s*"version":\s*")[^"]*(")/,
        `$1${next}$2`,
        `${PACKAGE_LOCK} 的 packages[""] version`,
      );
      edits.push({ rel: PACKAGE_LOCK, text: nextLock });
    }
  }

  const cargoToml = read(CARGO_TOML);
  const cargoNext = replaceOnce(
    cargoToml,
    /(\[package\][\s\S]*?\nversion\s*=\s*")[^"]*(")/,
    `$1${next}$2`,
    `${CARGO_TOML} 的 [package].version`,
  );
  if (cargoNext !== cargoToml) edits.push({ rel: CARGO_TOML, text: cargoNext });

  if (existsSync(abs(CARGO_LOCK))) {
    const lock = read(CARGO_LOCK);
    const pattern = new RegExp(
      `(\\[\\[package\\]\\]\\nname = "${CRATE_NAME}"\\nversion = ")[^"]*(")`,
    );
    const lockNext = replaceOnce(lock, pattern, `$1${next}$2`, `${CARGO_LOCK} 中 ${CRATE_NAME} 的 version`);
    if (lockNext !== lock) edits.push({ rel: CARGO_LOCK, text: lockNext });
  }

  return edits;
}

function applyEdits(edits) {
  for (const { rel, text } of edits) write(rel, text);
}

/**
 * 校验：所有载体必须一致，否则发版必然出问题
 * （漏一处会导致应用内版本号显示错，`--locked` 构建也会失败）。
 */
function verifyVersion(expected) {
  const found = {
    [TAURI_CONF]: currentVersion(),
    [PACKAGE_JSON]: JSON.parse(read(PACKAGE_JSON)).version,
    [CARGO_TOML]: (read(CARGO_TOML).match(/\[package\][\s\S]*?\nversion\s*=\s*"([^"]*)"/) ?? [])[1],
  };
  if (existsSync(abs(PACKAGE_LOCK))) {
    const lock = JSON.parse(read(PACKAGE_LOCK));
    found[`${PACKAGE_LOCK}(顶层)`] = lock.version;
    found[`${PACKAGE_LOCK}(packages[""])`] = lock.packages?.['']?.version;
  }
  if (existsSync(abs(CARGO_LOCK))) {
    found[CARGO_LOCK] = (read(CARGO_LOCK).match(
      new RegExp(`\\[\\[package\\]\\]\\nname = "${CRATE_NAME}"\\nversion = "([^"]*)"`),
    ) ?? [])[1];
  }

  const mismatched = Object.entries(found).filter(([, v]) => v !== expected);
  for (const [file, value] of Object.entries(found)) {
    console.log(`  ${value === expected ? '✓' : '✗'} ${file}: ${value ?? '(未找到)'}`);
  }
  if (mismatched.length > 0) {
    throw new Error(`版本号未全部对齐到 ${expected}`);
  }
}

function git(...gitArgs) {
  return execFileSync('git', gitArgs, { cwd: ROOT, encoding: 'utf8' }).trim();
}

function tryGit(...gitArgs) {
  try {
    return git(...gitArgs);
  } catch {
    return '';
  }
}

/**
 * 变更清单：上一个 tag 之后的所有提交标题。
 *
 * 为什么要有这个东西：`release.yml` 从**附注 tag 的正文**里取更新说明，并把它同时写进
 * GitHub Release 页面和 `latest.json` 的 `notes`。而 `latest.json` 的 notes 是应用内
 * 「应用更新」分区唯一的说明来源 —— 它一旦为空，前端会整块不渲染（`updates.ts` 里
 * `update.available && update.notes` 的判断），用户升级时看不到任何更新内容。
 *
 * 早先打的是**轻量 tag**（没有正文），于是只有「被打 tag 的那个提交」的正文能兜底；
 * 而发版提交本身是 `chore(release): ...` 一句话，正文为空 ⇒ notes 恒为 null。
 * 改成附注 tag 并把清单写进正文，这条链路才有内容可给。
 */
function changelogSincePreviousTag() {
  const previous = tryGit('describe', '--tags', '--abbrev=0', 'HEAD');
  const range = previous ? `${previous}..HEAD` : 'HEAD';
  return tryGit('log', range, '--no-merges', '--pretty=- %s')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    // 发版提交自己不该出现在清单里（它只是把版本号对齐）
    .filter((line) => !/^- chore\(release\)/.test(line));
}

/**
 * 人工写好的发布说明：`release-notes/<tag>.md`（例如 `release-notes/v0.2.0.md`）。
 *
 * 内容形状要求：**只有正文，不要写一级/二级标题** —— 它会整段塞进 Release 页面的
 * 「### 更新内容」下面，也会原样成为 `latest.json` 的 notes。所以用
 * 「引用块点题 + `- ` 清单」这种形状最贴，加了 `## 标题` 反而会把层级拧过来。
 *
 * 返回 null 表示没有人工文案，调用方退回提交标题清单。
 */
function curatedReleaseNotes(tag) {
  const file = `release-notes/${tag}.md`;
  if (!existsSync(abs(file))) return null;
  const text = read(file).trim();
  if (!text) return null;
  return text.split('\n');
}

// ── 主流程 ──────────────────────────────────────────────────────────────

const current = currentVersion();
const next = explicit ?? bumpPatch(current);
const tag = `v${next}`;

console.log(`版本：${current} → ${next}（tag ${tag}）`);

if (dryRun) {
  // 真正跑一遍正则，但不写盘：能提前暴露「某处版本字段匹配不到」
  const edits = computeVersionEdits(next);
  console.log('[dry-run] 将更新以下文件的版本号：');
  for (const { rel, text } of edits) {
    // 只报告真正变化的那一行，避免误抓到文件里其它形如 version 的依赖声明
    const before = read(rel).split('\n');
    const after = text.split('\n');
    const index = before.findIndex((line, i) => line !== after[i]);
    console.log(`  ${rel}`);
    console.log(`    旧: ${(before[index] ?? '').trim()}`);
    console.log(`    新: ${(after[index] ?? '').trim()}`);
  }
  console.log('[dry-run] 未修改任何文件、未提交、未打 tag。');
  process.exit(0);
}

const branch = git('rev-parse', '--abbrev-ref', 'HEAD');
if (branch !== 'main') {
  console.error(`当前分支是 ${branch}，发版请在 main 上进行。`);
  process.exit(1);
}

// 工作区必须干净：否则版本提交会把半成品一起带上去
const dirty = git('status', '--porcelain');
if (dirty) {
  console.error('工作区有未提交改动，请先提交或暂存：\n' + dirty);
  process.exit(1);
}

if (git('tag', '-l', tag)) {
  console.error(`tag ${tag} 已存在，请换一个版本号。`);
  process.exit(1);
}

const edits = computeVersionEdits(next);
applyEdits(edits);
const changed = edits.map((e) => e.rel);
console.log('已更新版本号的文件：');
console.log(changed.map((f) => `  ${f}`).join('\n') || '  (无)');

console.log('一致性校验：');
verifyVersion(next);

// 只提交明确列出的文件，绝不 `git add -A`
git('add', ...changed);
git('commit', '-m', `chore(release): kiro2api ${tag}`);

// 打**附注** tag：正文即发布说明，供 release.yml 写进 Release 页面与 latest.json。
// 注意消息必须「第一行一句标题 + 空行 + 正文」——`%(contents:body)` 取的是空行之后的部分，
// 整条消息都塞在第一行的话正文照样是空的。
const curated = curatedReleaseNotes(tag);
const changelog = curated ?? changelogSincePreviousTag();
const tagMessage = [`kiro2api ${tag}`, '', ...changelog].join('\n');
git('tag', '-a', tag, '-m', tagMessage);

console.log(`已提交并打附注 tag：${tag}`);
if (curated) {
  console.log(`发布说明来源：release-notes/${tag}.md（人工文案）`);
} else {
  console.warn(
    `⚠ 未找到 release-notes/${tag}.md，退化为提交标题清单 ——\n` +
      '  这段文字会直接显示给升级的用户，提交标题（fix(ci): …）不太适合。\n' +
      '  建议写一份 release-notes/' +
      tag +
      '.md 再发版（形状：引用块点题 + 「- 」清单，不要写 ## 标题）。'
  );
}
console.log('将随 Release 与 latest.json 一起发布的说明：');
console.log(changelog.map((l) => (l ? `  ${l}` : '')).join('\n') || '  (无)');
console.log('下一步：git push 与 git push origin ' + tag);

// 顺手把 tag 推到远端；普通提交由使用者自己决定何时 push
try {
  const remote = process.env.REMOTE || 'origin';
  execFileSync('git', ['push', remote, tag], { cwd: ROOT, stdio: 'inherit' });
  console.log(`已推送 tag 到 ${remote}，Release 工作流应已开始构建。`);
} catch {
  console.warn(`推送 tag 失败，请手动执行：git push origin ${tag}`);
}
