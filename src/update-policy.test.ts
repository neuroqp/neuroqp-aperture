import assert from "node:assert/strict";
import { test } from "node:test";
import {
  automaticCheckDue,
  announceUpdate,
  checkInterval,
  updatePreferences,
} from "./update-policy.ts";

test("quiet daily checks respect opt-out, damaged storage, and clock changes", () => {
  const defaults = updatePreferences("broken");
  assert.equal(automaticCheckDue(defaults, 100), true);
  const attempted = { ...defaults, lastAttempt: 100 };
  assert.equal(automaticCheckDue(attempted, 101), false);
  assert.equal(automaticCheckDue(attempted, checkInterval + 100), true);
  assert.equal(automaticCheckDue(attempted, 99), true);
  assert.equal(
    automaticCheckDue({ ...attempted, enabled: false }, checkInterval + 100),
    false,
  );
  assert.deepEqual(updatePreferences("null"), defaults);
  assert.deepEqual(
    updatePreferences('{"lastAttempt":"bad","deferredVersion":12}'),
    defaults,
  );
});

test("deferral hides that version automatically but manual checks and later versions remain visible", () => {
  const preferences = { ...updatePreferences(null), deferredVersion: "0.2.0" };
  assert.equal(announceUpdate("0.2.0", preferences, false), false);
  assert.equal(announceUpdate("0.2.0", preferences, true), true);
  assert.equal(announceUpdate("0.3.0", preferences, false), true);
});
