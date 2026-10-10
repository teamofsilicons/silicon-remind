/** Preserve one key while a JSON mutation's outcome is unknown. Selection/account changes reload the page. */
export class PendingMutations {
  private readonly pending = new Map<string, { key: string; at: number }>();

  begin(fingerprint: string): string {
    const now = Date.now();
    for (const [id, entry] of this.pending) if (now - entry.at > 600_000) this.pending.delete(id);
    const existing = this.pending.get(fingerprint);
    if (existing) return existing.key;
    if (this.pending.size >= 128) this.pending.delete(this.pending.keys().next().value!);
    const key = crypto.randomUUID();
    this.pending.set(fingerprint, { key, at: now });
    return key;
  }

  settle(fingerprint: string, status: number): void {
    if (status > 0 && status < 500 && status !== 408 && status !== 429) this.pending.delete(fingerprint);
  }
}
