<script>
  import { onMount } from 'svelte';

  let { data } = $props();

  let status = $state(null);
  let items = $state([]);
  let draft = $state('');
  let notice = $state('');
  let error = $state('');
  let sending = $state(false);

  const host = $derived(data.host || 'demo.localhost');
  const assetUrl = $derived(status?.asset_url ?? `http://assets.${host}/badge.svg`);
  const mailUrl = $derived(status?.mail_url ?? `http://mail.${host}/`);
  const health = $derived(status?.health ?? { postgres: false, redis: false, s3: false });
  const activity = $derived(status?.redis?.activity ?? 0);
  const world = $derived(status?.world ?? host);
  const branch = $derived(status?.branch ?? 'unknown');
  const scope = $derived(status?.fork ? `fork · ${status.fork}` : 'parent world');

  async function call(url, init) {
    const response = await fetch(url, init);
    const body = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(body.detail ?? `HTTP ${response.status}`);
    }
    return body;
  }

  async function refresh() {
    try {
      const [nextStatus, nextItems] = await Promise.all([
        call('/api/status'),
        call('/api/items')
      ]);
      status = nextStatus;
      items = nextItems;
      error = '';
    } catch (cause) {
      error = String(cause.message ?? cause);
    }
  }

  async function addItem(event) {
    event.preventDefault();
    const title = draft.trim();
    if (!title) return;
    try {
      await call('/api/items', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ title })
      });
      draft = '';
      await refresh();
    } catch (cause) {
      error = String(cause.message ?? cause);
    }
  }

  async function toggle(item) {
    try {
      await call(`/api/items/${item.id}`, { method: 'PATCH' });
      await refresh();
    } catch (cause) {
      error = String(cause.message ?? cause);
    }
  }

  async function remove(item) {
    try {
      await call(`/api/items/${item.id}`, { method: 'DELETE' });
      await refresh();
    } catch (cause) {
      error = String(cause.message ?? cause);
    }
  }

  async function sendEmail() {
    sending = true;
    notice = '';
    try {
      const body = await call('/api/test-email', { method: 'POST' });
      notice = `Delivered to ${body.delivered.join(', ')} — open the inbox`;
    } catch (cause) {
      error = String(cause.message ?? cause);
    } finally {
      sending = false;
    }
  }

  onMount(() => {
    refresh();
    const timer = setInterval(refresh, 2000);
    return () => clearInterval(timer);
  });
</script>

<svelte:head>
  <title>Slipfork demo · {world}</title>
</svelte:head>

<main>
  <header>
    <div class="identity">
      <span class="dot" class:live={status}></span>
      <span class="world">{world}</span>
      <span class="tag">{scope}</span>
      <span class="tag">branch {branch}</span>
    </div>
    <a class="inbox" href={mailUrl} target="_blank" rel="noreferrer">Mailpit inbox →</a>
  </header>

  <p class="lede">
    One world, one stack: PostgreSQL, Redis, two S3 buckets, a background worker and this SvelteKit
    frontend, all behind <code>{host}</code>. Forking copies code and data in one Btrfs snapshot.
  </p>

  {#if error}
    <p class="alert">{error}</p>
  {/if}
  {#if notice}
    <p class="ok">{notice}</p>
  {/if}

  <section class="grid">
    <article class="card">
      <h2>Dependencies</h2>
      <ul class="health">
        <li class:up={health.postgres}><span class="dot"></span>PostgreSQL <code>app_db</code></li>
        <li class:up={health.redis}><span class="dot"></span>Redis <code>cache</code></li>
        <li class:up={health.s3}><span class="dot"></span>S3 <code>assets</code></li>
      </ul>
      <p class="meta">{status?.counts?.items ?? 0} items · {status?.counts?.events ?? 0} worker events</p>
    </article>

    <article class="card">
      <h2>Worker activity</h2>
      <p class="counter">{activity}</p>
      <p class="meta">jobs handled in this world · heartbeat every 15s</p>
    </article>

    <article class="card">
      <h2>Public asset</h2>
      <img src={assetUrl} alt="Public-read asset served from the assets bucket" height="28" />
      <p class="meta"><code>assets.{host}</code> · public-read</p>
    </article>
  </section>

  <section class="card tasks">
    <h2>Tasks</h2>
    <form onsubmit={addItem}>
      <input bind:value={draft} placeholder="Add a task for this world" maxlength="120" />
      <button type="submit">Add</button>
    </form>

    <ul>
      {#each items as item (item.id)}
        <li class:done={item.done}>
          <button class="check" onclick={() => toggle(item)} aria-label="Toggle task">
            {item.done ? '✓' : '○'}
          </button>
          <span class="title">{item.title}</span>
          <button class="remove" onclick={() => remove(item)} aria-label="Delete task">×</button>
        </li>
      {:else}
        <li class="empty">No rows yet — the migration seeds two.</li>
      {/each}
    </ul>

    <p class="meta">
      Rows live in PostgreSQL <code>app_db</code>. Fork this world and they come along, isolated.
    </p>
  </section>

  <section class="card mail">
    <h2>Email</h2>
    <p class="meta">A real SMTP hand-off into Mailpit — no stub inbox.</p>
    <div class="row">
      <button onclick={sendEmail} disabled={sending}>
        {sending ? 'Sending…' : 'Send test email'}
      </button>
      <a href={mailUrl} target="_blank" rel="noreferrer">Open {mailUrl.replace(/^https?:\/\//, '')}</a>
    </div>
  </section>
</main>

<style>
  :global(body) {
    margin: 0;
    background: #070b14;
    color: #e2e8f0;
    font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif;
  }

  main {
    max-width: 980px;
    margin: 0 auto;
    padding: 2.5rem 1.25rem 3rem;
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
  }

  .identity {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.55rem;
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
  }

  .world {
    color: #22d3ee;
    font-weight: 700;
    font-size: 1rem;
  }

  .tag {
    background: #0f172a;
    border: 1px solid #1e293b;
    border-radius: 999px;
    padding: 0.2rem 0.65rem;
    color: #94a3b8;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #475569;
    display: inline-block;
  }

  .dot.live,
  .health li.up .dot {
    background: #22c55e;
  }

  .inbox {
    color: #22d3ee;
    text-decoration: none;
    font-size: 0.9rem;
  }

  .inbox:hover {
    text-decoration: underline;
  }

  .lede {
    margin: 0;
    color: #cbd5e1;
    max-width: 62ch;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 1rem;
  }

  .card {
    background: #0f172a;
    border: 1px solid #1e293b;
    border-radius: 0.85rem;
    padding: 1rem 1.15rem;
  }

  .card h2 {
    margin: 0 0 0.75rem;
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: #94a3b8;
  }

  .health {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    font-size: 0.9rem;
    color: #94a3b8;
  }

  .health li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .health li.up {
    color: #e2e8f0;
  }

  .counter {
    margin: 0;
    font-family: ui-monospace, monospace;
    font-size: 2.4rem;
    font-weight: 700;
    color: #f8fafc;
    line-height: 1;
  }

  .meta {
    margin: 0.6rem 0 0;
    color: #64748b;
    font-size: 0.82rem;
  }

  code {
    font-family: ui-monospace, monospace;
    color: #38bdf8;
  }

  .tasks form {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.9rem;
  }

  input {
    flex: 1;
    background: #070b14;
    border: 1px solid #1e293b;
    border-radius: 0.5rem;
    padding: 0.55rem 0.7rem;
    color: #e2e8f0;
    font: inherit;
  }

  input:focus {
    outline: 2px solid #0891b2;
    outline-offset: 1px;
  }

  button {
    background: #0891b2;
    border: none;
    border-radius: 0.5rem;
    padding: 0.55rem 0.9rem;
    color: #f0fdfa;
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  button:hover {
    background: #0e7490;
  }

  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }

  .tasks ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .tasks li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    border-bottom: 1px solid #1e293b;
    padding: 0.45rem 0;
    font-size: 0.92rem;
  }

  .tasks li:last-child {
    border-bottom: none;
  }

  .tasks li.done .title {
    color: #64748b;
    text-decoration: line-through;
  }

  .check,
  .remove {
    background: transparent;
    border: 1px solid #1e293b;
    color: #94a3b8;
    padding: 0.1rem 0.45rem;
    font-size: 0.95rem;
  }

  .remove:hover {
    background: #7f1d1d;
    color: #fee2e2;
  }

  .title {
    flex: 1;
  }

  .empty {
    color: #64748b;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.9rem;
  }

  .mail a {
    color: #22d3ee;
    font-size: 0.88rem;
    text-decoration: none;
    word-break: break-all;
  }

  .alert,
  .ok {
    margin: 0;
    padding: 0.7rem 0.9rem;
    border-radius: 0.6rem;
    font-size: 0.9rem;
  }

  .alert {
    background: #450a0a;
    border: 1px solid #7f1d1d;
    color: #fecaca;
  }

  .ok {
    background: #052e16;
    border: 1px solid #166534;
    color: #bbf7d0;
  }
</style>
