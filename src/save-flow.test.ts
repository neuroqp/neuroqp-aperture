import { test } from "node:test";
import assert from "node:assert/strict";
import { saveAnnotationFile, type AnnotationTarget } from "./save-flow.ts";

const target: AnnotationTarget = {
  path: "/tmp/cells étude.tif",
  exists: true,
  isTiff: true,
  warning: "Open in another window.",
  acknowledgment: "initial",
};

test("cancelled conflict and failed saves do not report completed saves", async () => {
  let writes = 0;
  const actions = {
    choose: async () => "cancel",
    pick: async () => null,
    inspect: async () => target,
    write: async () => {
      writes++;
      throw new Error("disk full");
    },
  };
  assert.equal(await saveAnnotationFile(target, actions), null);
  assert.equal(writes, 0);
  actions.choose = async () => "overwrite";
  await assert.rejects(saveAnnotationFile(target, actions), /disk full/);
});

test("Save as inspects an existing destination and can cancel its warning", async () => {
  const choices: string[] = [];
  assert.equal(
    await saveAnnotationFile(target, {
      choose: async (current) => {
        choices.push(current.path);
        return choices.length === 1 ? "save-as" : "cancel";
      },
      pick: async () => "/tmp/existing.tif",
      inspect: async (path) => ({
        ...target,
        path,
        acknowledgment: "existing",
      }),
      write: async () => {
        throw new Error("must not write");
      },
    }),
    null,
  );
  assert.deepEqual(choices, [target.path, "/tmp/existing.tif"]);
});

test("a changed destination requires a fresh explicit acknowledgment", async () => {
  const acknowledgments: (string | null)[] = [];
  const choices: string[] = [];
  const path = await saveAnnotationFile(target, {
    choose: async (current) => {
      choices.push(current.acknowledgment);
      return "overwrite";
    },
    pick: async () => null,
    inspect: async () => target,
    write: async (_path, overwrite, acknowledgment) => {
      assert.equal(overwrite, true);
      acknowledgments.push(acknowledgment);
      return acknowledgments.length === 1
        ? { saved: false, warning: "Changed again.", acknowledgment: "new" }
        : { saved: true, warning: null, acknowledgment: "new" };
    },
  });
  assert.equal(path, target.path);
  assert.deepEqual(choices, ["initial", "new"]);
  assert.deepEqual(acknowledgments, ["initial", "new"]);
});
