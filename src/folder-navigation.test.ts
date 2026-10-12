import assert from "node:assert/strict";
import { test } from "node:test";
import { FolderNavigation } from "./folder-navigation.ts";

test("folder selection advances immediately through rapid clicks and stops at either end", async () => {
  const paths = ["/images/a.png", "/images/b.tif", "/images/c.jpg"];
  let reads = 0;
  const navigation = new FolderNavigation(
    async () => {
      reads++;
      return paths;
    },
    () => {},
    assert.fail,
  );
  assert.equal(navigation.adjacent(1), null);
  navigation.select(paths[0]);
  await Promise.resolve();
  assert.equal(navigation.adjacent(-1), null);
  navigation.select(navigation.adjacent(1)!);
  navigation.select(navigation.adjacent(1)!);
  assert.equal(navigation.adjacent(1), null);
  assert.equal(navigation.adjacent(-1), paths[1]);
  navigation.select(navigation.adjacent(-1)!);
  assert.equal(navigation.adjacent(-1), paths[0]);
  assert.equal(reads, 1);
});

test("late folder replies and errors cannot replace a newer selection", async () => {
  let resolveOld!: (paths: string[]) => void;
  let rejectOther!: (error: Error) => void;
  const failures: string[] = [];
  const navigation = new FolderNavigation(
    (path) =>
      path === "/old/a.png"
        ? new Promise((resolve) => {
            resolveOld = resolve;
          })
        : path === "/other/a.png"
          ? new Promise((_, reject) => {
              rejectOther = reject;
            })
          : Promise.resolve(["/new/a.png", "/new/b.png"]),
    () => {},
    (error) => failures.push(error),
  );
  navigation.select("/old/a.png");
  navigation.select("/other/a.png");
  navigation.select("/new/a.png");
  await Promise.resolve();
  resolveOld(["/old/a.png", "/old/b.png"]);
  rejectOther(new Error("Unreadable old folder"));
  await Promise.resolve();
  assert.equal(navigation.adjacent(1), "/new/b.png");
  assert.deepEqual(failures, []);
  navigation.select("/new/missing.png");
  assert.equal(navigation.adjacent(1), null);
  await Promise.resolve();
  assert.equal(navigation.adjacent(-1), null);
});
