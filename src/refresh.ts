export type ReadTask = { name: string; read: () => Promise<unknown>; update: (value: never) => void };
type Slot = {
  task: ReadTask;
  generation: number;
  reading?: { generation: number; timer: ReturnType<typeof setTimeout> };
  again: boolean;
  failures: number;
  retryAt: number;
};
// Each module owns its in-flight read. A hung optional IPC never holds the other modules or actions.
export function createReadCoordinator(status: (name: string, error: string | null) => void, timeoutMs = 5000) {
  const slots = new Map<string, Slot>();
  let disposed = false;
  function start(slot: Slot) {
    slot.again = false;
    const generation = slot.generation;
    const task = slot.task;
    function finish(deliver: () => void, failed: boolean) {
      if (disposed || slot.reading !== reading) return;
      clearTimeout(reading.timer);
      slot.reading = undefined;
      if (slot.generation === generation) {
        slot.failures = failed ? Math.min(slot.failures + 1, 4) : 0;
        slot.retryAt = failed ? Date.now() + 1000 * 2 ** (slot.failures - 1) : 0;
        try { deliver(); }
        catch (error) { status(task.name, String(error)); }
      }
      if (slot.again && !slot.reading) start(slot);
    }
    const reading = {
      generation,
      timer: setTimeout(() => finish(() => status(task.name, '读取超时，保留上次数据'), true), timeoutMs),
    };
    slot.reading = reading;
    void Promise.resolve().then(task.read).then(
      value => finish(() => { task.update(value as never); status(task.name, null); }, false),
      error => finish(() => status(task.name, String(error)), true),
    );
  }
  return {
    refresh(tasks: ReadTask[]) {
      if (disposed) return;
      for (const task of tasks) {
        let slot = slots.get(task.name);
        if (!slot) { slot = { task, generation: 0, again: false, failures: 0, retryAt: 0 }; slots.set(task.name, slot); }
        slot.task = task;
        if (slot.reading) {
          if (slot.generation !== slot.reading.generation) slot.again = true;
        } else if (Date.now() >= slot.retryAt) start(slot);
      }
    },
    invalidate() {
      for (const slot of slots.values()) {
        slot.generation++;
        slot.again = false;
        slot.retryAt = 0;
      }
    },
    dispose() {
      disposed = true;
      for (const { reading } of slots.values()) if (reading) clearTimeout(reading.timer);
      slots.clear();
    },
  };
}
