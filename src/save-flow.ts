export type AnnotationTarget = {
  path: string;
  exists: boolean;
  isTiff: boolean;
  warning: string | null;
  acknowledgment: string;
};

export type SaveResult = {
  saved: boolean;
  warning: string | null;
  acknowledgment: string;
};

export async function saveAnnotationFile(
  initial: AnnotationTarget,
  actions: {
    choose: (target: AnnotationTarget) => Promise<string>;
    pick: (path: string) => Promise<string | null>;
    inspect: (path: string) => Promise<AnnotationTarget>;
    write: (
      path: string,
      overwrite: boolean,
      acknowledgment: string | null,
    ) => Promise<SaveResult>;
  },
): Promise<string | null> {
  let target = initial;
  let overwrite = false;
  let acknowledgment: string | null = null;
  let ask = target.isTiff || target.exists;
  for (;;) {
    if (ask || target.warning) {
      const choice = await actions.choose(target);
      if (choice === "save-as") {
        const path = await actions.pick(target.path);
        if (!path) return null;
        target = await actions.inspect(path);
        overwrite = true;
        acknowledgment = null;
        ask = false;
        continue;
      }
      if (choice !== "overwrite") return null;
      overwrite = true;
      acknowledgment = target.warning ? target.acknowledgment : null;
    }
    if (
      !/\.tiff?$/i.test(target.path) &&
      !(target.isTiff && target.path === initial.path)
    )
      throw new Error("Use a .tif or .tiff filename for the annotated TIFF.");
    const result = await actions.write(target.path, overwrite, acknowledgment);
    if (result.saved) return target.path;
    target = { ...target, ...result };
    ask = true;
  }
}
