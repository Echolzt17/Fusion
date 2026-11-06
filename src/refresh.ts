export type ReadTask = { name: string; read: () => Promise<unknown>; update: (value: never) => void };
type Slot = { task: ReadTask; generation: number; runningGeneration: number; pending: boolean; again: boolean };
// Each module owns its in-flight read. A hung optional IPC never holds the other modules or actions.
export function createReadCoordinator(status: (name: string, error: string | null) => void, timeoutMs = 5000) {
  const slots = new Map<string, Slot>();
  let disposed = false;
  function start(slot: Slot) {
    slot.pending = true; slot.again = false;
    const generation = slot.generation;
    slot.runningGeneration = generation;
    const task = slot.task;
    let expired = false;
    const valid = () => !disposed && !expired && slot.generation === generation;
    const timer = setTimeout(() => { if (valid()) { status(task.name, '读取超时，保留上次数据'); expired = true; } }, timeoutMs);
    void Promise.resolve().then(task.read).then(value => {
      if (valid()) { task.update(value as never); status(task.name, null); }
    }).catch(error => { if (valid()) status(task.name, String(error)); }).finally(() => {
      clearTimeout(timer); slot.pending = false;
      if (slot.again && !disposed) start(slot);
    });
  }
  return {
    refresh(tasks: ReadTask[]) {
      if (disposed) return;
      for (const task of tasks) {
        let slot = slots.get(task.name);
        if (!slot) { slot = { task, generation: 0, runningGeneration: 0, pending: false, again: false }; slots.set(task.name, slot); }
        slot.task = task;
        if (!slot.pending) start(slot);
        else if (slot.generation !== slot.runningGeneration) slot.again = true;
      }
    },
    invalidate() { for (const slot of slots.values()) slot.generation++; },
    dispose() { disposed = true; for (const slot of slots.values()) slot.generation++; },
  };
}
