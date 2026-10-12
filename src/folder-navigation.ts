/** Keep selection ahead of load replies so rapid clicks advance from the latest target. */
export class FolderNavigation {
  private path: string | null = null;
  private paths: string[] = [];
  private generation = 0;
  private read: (path: string) => Promise<string[]>;
  private changed: () => void;
  private failed: (message: string) => void;

  constructor(
    read: (path: string) => Promise<string[]>,
    changed: () => void,
    failed: (message: string) => void,
  ) {
    this.read = read;
    this.changed = changed;
    this.failed = failed;
  }

  adjacent(direction: -1 | 1) {
    const index = this.path === null ? -1 : this.paths.indexOf(this.path);
    return index < 0 ? null : (this.paths[index + direction] ?? null);
  }

  select(path: string) {
    this.path = path;
    const generation = ++this.generation;
    if (this.paths.includes(path)) {
      this.changed();
      return;
    }
    this.paths = [];
    this.changed();
    void this.read(path).then(
      (paths) => {
        if (generation !== this.generation) return;
        this.paths = paths;
        this.changed();
      },
      (error) => {
        if (generation === this.generation) this.failed(String(error));
      },
    );
  }
}
