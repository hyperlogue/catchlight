/** Remove test fixtures without masking persistent filesystem failures. */
import { rm } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";

export async function removeTemporaryDirectory(path: string, remove: typeof rm = rm): Promise<void> {
  // Bun's async recursive rm has reported EFAULT in CI teardown. A retry
  // can finish a partially removed tree; persistent failures still surface.
  for (let attempt = 0; ; attempt++) {
    try {
      await remove(path, { recursive: true, force: true });
      return;
    } catch (cause) {
      if (!(cause instanceof Error) || !("code" in cause) || cause.code !== "EFAULT" || attempt === 3) {
        throw cause;
      }
      await delay(25 * (attempt + 1));
    }
  }
}
