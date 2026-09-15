/// Keep asset-pane results ordered when directory changes overlap asynchronous
/// backend work. Only the latest load may update the UI.
export function createAssetLoader(listImages) {
  let generation = 0;

  return {
    invalidate() {
      generation += 1;
    },

    async load(baseDir) {
      const request = ++generation;
      try {
        const list = await listImages(baseDir);
        if (request !== generation) return { kind: "stale" };
        return { kind: "ready", list };
      } catch (error) {
        if (request !== generation) return { kind: "stale" };
        return { kind: "failed", error };
      }
    },
  };
}

