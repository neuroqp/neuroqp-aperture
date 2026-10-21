import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const severities = ["info", "low", "moderate", "high", "critical"];

// pnpm 9 exits with 1 for findings. Registry/CLI failures must still fail CI.
function parseReport(stdout, status) {
  const report = JSON.parse(stdout);
  const counts = report.metadata?.vulnerabilities;
  if (
    ![0, 1].includes(status) ||
    report.error ||
    !report.advisories ||
    Array.isArray(report.advisories) ||
    typeof report.advisories !== "object" ||
    !severities.every(
      (level) => Number.isInteger(counts?.[level]) && counts[level] >= 0,
    ) ||
    (status === 1 && severities.every((level) => counts[level] === 0)) ||
    (Object.keys(report.advisories).length === 0) !==
      severities.every((level) => counts[level] === 0) ||
    !Object.values(report.advisories).every(
      (advisory) =>
        severities.includes(advisory.severity) &&
        typeof advisory.module_name === "string",
    )
  ) {
    throw new Error(
      "Audit did not return a complete pnpm 9 vulnerability report",
    );
  }
  return report;
}

function blocksProduction(report) {
  const counts = report.metadata.vulnerabilities;
  return counts.high + counts.critical > 0;
}

// Escape instead of stripping so version ranges such as ">=1.2.3" stay readable.
function tableCell(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\|/g, "\\|")
    .replace(/[\r\n]/g, " ");
}

function summary(scope, report) {
  const counts = report.metadata.vulnerabilities;
  const lines = [
    `### ${scope} dependencies`,
    "",
    severities.map((level) => `${level}: ${counts[level]}`).join(" · "),
    "",
    "| Package | Severity | Advisory | Patched versions |",
    "| --- | --- | --- | --- |",
  ];
  for (const advisory of Object.values(report.advisories)) {
    lines.push(
      `| ${[
        advisory.module_name,
        advisory.severity,
        advisory.url ?? advisory.id,
        advisory.patched_versions === "<0.0.0"
          ? "No published fix"
          : advisory.patched_versions,
      ]
        .map(tableCell)
        .join(" | ")} |`,
    );
  }
  return lines.join("\n");
}

if (process.argv[2] === "--self-test") {
  const fixture = (level) => ({
    advisories: level
      ? { example: { module_name: "example", severity: level } }
      : {},
    metadata: {
      vulnerabilities: Object.fromEntries(
        severities.map((s) => [s, s === level ? 1 : 0]),
      ),
    },
  });
  for (const level of [undefined, ...severities]) {
    const report = parseReport(JSON.stringify(fixture(level)), level ? 1 : 0);
    assert.equal(
      blocksProduction(report),
      level === "high" || level === "critical",
    );
  }
  for (const [stdout, status] of [
    ["not JSON", 1],
    [JSON.stringify({ error: { code: "ECONNRESET" } }), 1],
    [JSON.stringify(fixture()), 1],
    [JSON.stringify(fixture()), 2],
    [JSON.stringify(fixture()), null],
    [JSON.stringify({ advisories: {} }), 0],
    [JSON.stringify({ ...fixture("high"), advisories: {} }), 1],
  ]) {
    assert.throws(() => parseReport(stdout, status));
  }
  const unfixable = fixture("high");
  unfixable.advisories.example.patched_versions = "<0.0.0";
  assert.match(summary("All", unfixable), /No published fix/);
  assert.equal(tableCell("bad|name\n<tag>"), "bad\\|name &lt;tag&gt;");
  console.log("Audit policy checks passed");
} else {
  const directory = process.argv[2];
  assert.ok(
    directory,
    "Usage: node scripts/security-audit.mjs <report-directory>",
  );
  mkdirSync(directory, { recursive: true });
  const sections = [
    "## Dependency security audit",
    "High/critical production findings fail this check. All findings, including development-only and unfixable advisories, remain visible. Audit errors also fail the check.",
  ];
  for (const [scope, flags] of [
    ["Production", ["--prod"]],
    ["All", []],
  ]) {
    const result = spawnSync("pnpm", ["audit", ...flags, "--json"], {
      encoding: "utf8",
      timeout: 120_000,
      maxBuffer: 10 * 1024 * 1024,
    });
    writeFileSync(
      join(directory, `${scope.toLowerCase()}.json`),
      result.stdout ?? "",
    );
    writeFileSync(
      join(directory, `${scope.toLowerCase()}.stderr.txt`),
      result.stderr ?? "",
    );
    try {
      const report = parseReport(result.stdout, result.status);
      sections.push(summary(scope, report));
      if (scope === "Production" && blocksProduction(report))
        process.exitCode = 1;
    } catch (error) {
      sections.push(`### ${scope} audit failed`, String(result.error ?? error));
      process.exitCode = 1;
    }
  }
  const markdown = sections.join("\n\n") + "\n";
  writeFileSync(join(directory, "summary.md"), markdown);
  if (process.env.GITHUB_STEP_SUMMARY)
    appendFileSync(process.env.GITHUB_STEP_SUMMARY, markdown);
  console.log(markdown);
}
