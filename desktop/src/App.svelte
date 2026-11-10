<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { invoke } from '@tauri-apps/api/core';
  import { open } from '@tauri-apps/plugin-dialog';
  import mark from './mark.svg?no-inline';

  interface Repository {
    id: string;
    path: string;
    group_id: number | null;
  }

  interface Status {
    repository: Repository;
    branch: string | null;
    upstream: string | null;
    ahead: number | null;
    behind: number | null;
    changes: number;
    untracked: number;
    problem: string | null;
  }

  interface Group {
    id: number;
    name: string;
  }

  type GroupView = 'all' | 'ungrouped' | number;

  interface RemoteInfo {
    name: string;
    fetch_urls: string[];
    push_urls: string[];
  }

  interface ScanCandidate {
    name: string;
    path: string;
    registered: boolean;
  }

  interface ScanResult {
    candidates: ScanCandidate[];
    skipped: number;
  }

  interface SyncReview {
    branch: string;
    head: string;
    fingerprint: string;
    upstream: string;
    tracked: number;
    ahead: number;
    needs_push: boolean;
    untracked: string[];
  }

  interface SyncItem {
    repository: Repository;
    review: SyncReview | null;
    problem: string | null;
  }

  interface SyncApproval {
    repository: Repository;
    review: SyncReview;
    include: string[];
  }

  let rows = $state<Status[]>([]);
  let selected = $state<string[]>([]);
  let busy = $state(true);
  let error = $state('');
  let scanning = $state(false);
  let previewDialog: HTMLDialogElement;
  let scanResult = $state<ScanResult | null>(null);
  let previewError = $state('');
  const scanSelection = new SvelteSet<string>();
  const newCandidates = $derived(scanResult?.candidates.filter((candidate) => !candidate.registered) ?? []);
  let syncDialog: HTMLDialogElement;
  let syncScope: Repository[] = [];
  let syncItems = $state<SyncItem[]>([]);
  let syncError = $state('');
  let syncChecking = $state(false);
  const syncSelection = new SvelteSet<string>();
  const canConfirmSync = $derived(syncItems.some((item) => item.review !== null));
  const willPush = $derived(syncSelection.size > 0 || syncItems.some((item) => item.review?.needs_push));
  const syncFileKey = (repo: Repository, file: string) => JSON.stringify([repo.path, file]);

  let infoDialog: HTMLDialogElement;
  let infoRow = $state<Status | null>(null);
  let infoRemotes = $state<RemoteInfo[] | null>(null);
  let infoError = $state('');
  let infoRequest = 0;

  let confirmDiskRemoval = $state(false);
  let cloneDialog: HTMLDialogElement;
  let cloneAddress = $state('');
  let cloneParent = $state('');
  let cloneName = $state('');
  let cloneNameEdited = false;
  let cloneError = $state('');
  let cloning = $state(false);

  let groups = $state<Group[]>([]);
  let activeGroup = $state<GroupView>('all');
  let draggedRepository = $state<Repository | null>(null);
  let dropTarget = $state<number | null | undefined>(undefined);
  let groupDialog: HTMLDialogElement;
  let groupName = $state('');
  let groupError = $state('');

  const groupCounts = $derived.by(() => {
    const counts = new Map<number | null, number>();
    for (const { repository } of rows) {
      counts.set(repository.group_id, (counts.get(repository.group_id) ?? 0) + 1);
    }
    return counts;
  });
  const visibleRows = $derived(rows.filter(({ repository }) =>
    activeGroup === 'all' || repository.group_id === (activeGroup === 'ungrouped' ? null : activeGroup)
  ));
  const visibleSelection = $derived(visibleRows.filter(({ repository }) => selected.includes(repository.id)));
  const allSelected = $derived(visibleRows.length > 0 && visibleSelection.length === visibleRows.length);

  function hasLocalWork(row: Status) {
    return row.changes > 0 || row.untracked > 0 || (row.ahead ?? 0) > 0;
  }

  function repositoryState(row: Status) {
    if (row.problem || (row.behind && hasLocalWork(row))) return { tone: 'blocked', label: '需要处理' };
    if (row.behind) return { tone: 'incoming', label: '待拉取' };
    if (row.changes > 0 || row.ahead) return { tone: 'pending', label: '待推送' };
    if (row.untracked > 0) return { tone: 'pending', label: '有未跟踪文件' };
    if (row.ahead === null || row.behind === null) return { tone: 'unknown', label: '尚未获取远端状态' };
    return { tone: 'clean', label: '无待同步变更' };
  }

  async function load() {
    const known = new Map(rows.map(({ repository }) => [repository.id, repository.path]));
    const dashboard = await invoke<{ repositories: Status[]; groups: Group[] }>('repository_status');
    const next = dashboard.repositories;
    groups = dashboard.groups;
    if (typeof activeGroup === 'number' && !groups.some((group) => group.id === activeGroup)) activeGroup = 'all';
    const available = new Map(next.map(({ repository }) => [repository.id, repository.path]));
    selected = selected.filter((id) => available.has(id) && available.get(id) === known.get(id));
    selected.push(...next.filter(({ repository }) => !known.has(repository.id)).map(({ repository }) => repository.id));
    rows = next;
  }

  async function refresh() {
    busy = true;
    error = '';
    try {
      await load();
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  onMount(() => { void refresh(); });

  function startSync() {
    if (busy || visibleSelection.length === 0) return;
    syncScope = visibleSelection.map(({ repository }) => repository);
    syncItems = [];
    syncSelection.clear();
    syncDialog.showModal();
    void checkSync();
  }

  async function checkSync(approvals?: SyncApproval[]) {
    if (busy) return;
    busy = true;
    syncChecking = true;
    syncError = '';
    try {
      const report = await invoke<{ items: SyncItem[] }>('sync_repositories', {
        repositories: syncScope,
        approvals,
      });
      syncItems = report.items;
      syncSelection.clear();
      await load();
      if (syncItems.length === 0) syncDialog.close();
    } catch (cause) {
      syncError = String(cause);
    } finally {
      syncChecking = false;
      busy = false;
    }
  }

  function confirmSync() {
    const approvals: SyncApproval[] = [];
    for (const { repository, review } of syncItems) {
      if (!review) continue;
      approvals.push({
        repository,
        review,
        include: review.untracked.filter((file) => syncSelection.has(syncFileKey(repository, file))),
      });
    }
    void checkSync(approvals);
  }

  async function scan() {
    busy = true;
    error = '';
    try {
      const paths = await open({ title: '选择要扫描的目录', directory: true, multiple: true });
      if (!paths?.length) return;
      scanning = true;
      scanResult = await invoke<ScanResult>('scan_repositories', { paths });
      previewError = '';
      selectAllCandidates(true);
      previewDialog.showModal();
    } catch (cause) {
      error = String(cause);
    } finally {
      scanning = false;
      busy = false;
    }
  }

  function selectAllCandidates(checked: boolean) {
    scanSelection.clear();
    if (checked) {
      for (const candidate of newCandidates) scanSelection.add(candidate.path);
    }
  }

  async function addSelected() {
    if (busy || scanSelection.size === 0) return;
    busy = true;
    previewError = '';
    try {
      await invoke('add_scanned_repositories', { paths: [...scanSelection] });
    } catch (cause) {
      previewError = String(cause);
      busy = false;
      return;
    }
    previewDialog.close();
    await refresh();
  }

  function toggleVisibleSelection() {
    const visible = new Set(visibleRows.map(({ repository }) => repository.id));
    selected = allSelected
      ? selected.filter((id) => !visible.has(id))
      : [...new Set([...selected, ...visible])];
  }

  function showCreateGroup() {
    groupName = '';
    groupError = '';
    groupDialog.showModal();
  }

  async function createGroup(event: SubmitEvent) {
    event.preventDefault();
    if (busy || !groupName.trim()) return;
    busy = true;
    groupError = '';
    try {
      const group = await invoke<Group>('create_group', { name: groupName });
      groups = [...groups, group];
      groupDialog.close();
    } catch (cause) {
      groupError = String(cause);
    } finally {
      busy = false;
    }
  }

  async function moveToGroup(repository: Repository, groupId: number | null) {
    if (busy || repository.group_id === groupId) return;
    busy = true;
    error = '';
    if (infoDialog.open) infoError = '';
    try {
      const updated = await invoke<Repository>('assign_repository_group', { repository, groupId });
      rows = rows.map((row) => row.repository.id === updated.id
        ? { ...row, repository: updated }
        : row);
      if (infoRow?.repository.id === updated.id) infoRow = { ...infoRow, repository: updated };
    } catch (cause) {
      if (infoDialog.open) infoError = String(cause);
      else error = String(cause);
    } finally {
      busy = false;
    }
  }

  function startRepositoryDrag(event: DragEvent, repository: Repository) {
    if (busy || !event.dataTransfer) {
      event.preventDefault();
      return;
    }
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', repository.id);
    draggedRepository = repository;
  }

  function finishRepositoryDrag() {
    draggedRepository = null;
    dropTarget = undefined;
  }

  function allowGroupDrop(event: DragEvent, groupId: number | null) {
    if (busy || !draggedRepository || draggedRepository.group_id === groupId || !event.dataTransfer) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'move';
    dropTarget = groupId;
  }

  function dropIntoGroup(event: DragEvent, groupId: number | null) {
    if (!draggedRepository) return;
    event.preventDefault();
    const repository = draggedRepository;
    finishRepositoryDrag();
    void moveToGroup(repository, groupId);
  }

  function showClone() {
    cloneAddress = '';
    cloneParent = '';
    cloneName = '';
    cloneNameEdited = false;
    cloneError = '';
    cloneDialog.showModal();
  }

  function updateCloneAddress(event: Event & { currentTarget: HTMLInputElement }) {
    cloneAddress = event.currentTarget.value;
    if (cloneNameEdited) return;
    const path = cloneAddress.trim().split(/[?#]/, 1)[0].replace(/[\\/]+$/, '');
    cloneName = path.split(/[\\/:]/).at(-1)?.replace(/\.git$/i, '') ?? '';
  }

  async function chooseCloneParent() {
    busy = true;
    cloneError = '';
    try {
      const path = await open({ title: '选择克隆保存位置', directory: true });
      if (path) cloneParent = path;
    } catch (cause) {
      cloneError = String(cause);
    } finally {
      busy = false;
    }
  }

  async function cloneRepository(event: SubmitEvent) {
    event.preventDefault();
    if (busy || !cloneAddress.trim() || !cloneParent.trim() || !cloneName.trim()) return;
    busy = true;
    cloning = true;
    cloneError = '';
    error = '';
    try {
      await invoke('clone_repository', { address: cloneAddress, parent: cloneParent.trim(), name: cloneName });
      cloneDialog.close();
      await load();
    } catch (cause) {
      if (cloneDialog.open) cloneError = String(cause);
      else error = String(cause);
    } finally {
      cloning = false;
      busy = false;
    }
  }

  async function showInfo(row: Status) {
    const request = ++infoRequest;
    infoRow = row;
    infoRemotes = null;
    infoError = '';
    confirmDiskRemoval = false;
    infoDialog.showModal();
    try {
      const remotes = await invoke<RemoteInfo[]>('repository_remotes', { repository: row.repository });
      if (request === infoRequest) infoRemotes = remotes;
    } catch (cause) {
      if (request === infoRequest) infoError = String(cause);
    }
  }

  async function remove(repository: Repository, fromDisk = false) {
    busy = true;
    infoError = '';
    error = '';
    try {
      await invoke(fromDisk ? 'trash_repository' : 'remove_repository', { repository });
      infoDialog.close();
      await load();
    } catch (cause) {
      if (infoDialog.open) infoError = String(cause);
      else error = String(cause);
    } finally {
      busy = false;
    }
  }

  function toggle(id: string) {
    selected = selected.includes(id) ? selected.filter((item) => item !== id) : [...selected, id];
  }

</script>

<header class="dashboard-header">
  <div class="toolbar">
    <div class="toolbar-start">
      <img src={mark} alt="" width="29" height="29" />
      <label class="selection">
        <input
          type="checkbox"
          aria-label="全选当前仓库"
          checked={allSelected}
          indeterminate={visibleSelection.length > 0 && !allSelected}
          disabled={busy || visibleRows.length === 0}
          onchange={toggleVisibleSelection}
        />
        已选 {visibleSelection.length} 个
      </label>
    </div>
    <div class="actions">
      <button class="secondary" onclick={refresh} disabled={busy}>刷新</button>
      <button class="add-button" onclick={scan} disabled={busy}>添加仓库</button>
      <button class="secondary" onclick={showClone} disabled={busy}>克隆仓库</button>
      <button class="primary" onclick={startSync} disabled={busy || visibleSelection.length === 0}>同步</button>
    </div>
  </div>
</header>

<div class="dashboard-layout">
  <aside class="groups-sidebar" aria-label="仓库分组">
    <div class="groups-heading">
      <h2>分组</h2>
      <button class="add-group" aria-label="添加分组" title="添加分组" onclick={showCreateGroup} disabled={busy}>＋</button>
    </div>
    <nav aria-label="切换分组">
      <button class="group-link" aria-current={activeGroup === 'all' ? 'true' : undefined}
        onclick={() => activeGroup = 'all'} disabled={busy}>
        <span>全部仓库</span><small>{rows.length}</small>
      </button>
      <button class="group-link" aria-current={activeGroup === 'ungrouped' ? 'true' : undefined}
        class:drop-hover={dropTarget === null}
        onclick={() => activeGroup = 'ungrouped'} disabled={busy}
        ondragover={(event) => allowGroupDrop(event, null)}
        ondrop={(event) => dropIntoGroup(event, null)}>
        <span>未分组</span><small>{groupCounts.get(null) ?? 0}</small>
      </button>
      {#each groups as group (group.id)}
        <button class="group-link" aria-current={activeGroup === group.id ? 'true' : undefined}
          class:drop-hover={dropTarget === group.id}
          onclick={() => activeGroup = group.id} disabled={busy}
          ondragover={(event) => allowGroupDrop(event, group.id)}
          ondrop={(event) => dropIntoGroup(event, group.id)}
          title={group.name}>
          <span>{group.name}</span><small>{groupCounts.get(group.id) ?? 0}</small>
        </button>
      {/each}
    </nav>
  </aside>

  <main class="repository-panel" aria-label="仓库列表">
    {#if error}
      <p class="banner error" role="alert">{error}</p>
    {/if}

    {#if scanning}
      <p class="banner" role="status">正在扫描所选目录及其子目录…</p>
    {/if}

    {#if visibleRows.length === 0}
      <div class="empty">
        <img src={mark} alt="" width="48" height="48" />
        {#if activeGroup === 'all'}
          <h2>{busy ? '正在加载仓库…' : '添加你的第一个仓库'}</h2>
          <p>选择仓库目录或上级目录，扫描后勾选要添加的仓库。</p>
          <button class="primary" onclick={scan} disabled={busy}>选择目录</button>
        {:else}
          <h2>{activeGroup === 'ungrouped' ? '没有未分组的仓库' : '这个分组还没有仓库'}</h2>
          <p>从「全部仓库」把卡片拖到左侧对应分组。</p>
          <button class="secondary" onclick={() => activeGroup = 'all'} disabled={busy}>查看全部仓库</button>
        {/if}
      </div>
    {:else}
      <ul class="repo-grid">
        {#each visibleRows as row (row.repository.id)}
          {@const state = repositoryState(row)}
          {@const chosen = selected.includes(row.repository.id)}
          <li class="repo-card" class:chosen data-tone={state.tone}
            draggable={!busy}
            ondragstart={(event) => startRepositoryDrag(event, row.repository)}
            ondragend={finishRepositoryDrag}>
            <button
              class="repo-select"
              aria-label={`${row.repository.id}，${state.label}`}
              aria-pressed={chosen}
              title={`${row.repository.id} · ${state.label}`}
              disabled={busy}
              onclick={() => toggle(row.repository.id)}
            >
              <span class="status-dot" aria-hidden="true"></span>
              <strong>{row.repository.id}</strong>
            </button>
            <button
              class="repo-info"
              aria-label={`查看 ${row.repository.id} 信息`}
              title="仓库信息"
              disabled={busy}
              onclick={() => showInfo(row)}
            >
              <svg viewBox="0 0 20 20" width="18" height="18" fill="none" aria-hidden="true">
                <circle cx="10" cy="10" r="7.5" stroke="currentColor" stroke-width="1.5" />
                <path d="M10 9v4.2" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
                <circle cx="10" cy="6.7" r=".9" fill="currentColor" />
              </svg>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </main>
</div>

<dialog
  class="scan-dialog"
  aria-labelledby="group-title"
  bind:this={groupDialog}
  oncancel={(event) => { if (busy) event.preventDefault(); }}
>
  <div class="scan-header">
    <h2 id="group-title">添加分组</h2>
    <p>添加后，把仓库卡片拖到左侧的新分组。</p>
  </div>
  <form onsubmit={createGroup}>
    <div class="clone-fields">
      <label for="group-name">分组名称</label>
      <input id="group-name" type="text" bind:value={groupName} maxlength="60" required disabled={busy} />
    </div>
    {#if groupError}<p class="banner error" role="alert">{groupError}</p>{/if}
    <div class="scan-footer">
      <button class="secondary" type="button" onclick={() => groupDialog.close()} disabled={busy}>取消</button>
      <button class="primary" type="submit" disabled={busy || !groupName.trim()}>创建分组</button>
    </div>
  </form>
</dialog>

<dialog
  class="scan-dialog"
  aria-labelledby="clone-title"
  bind:this={cloneDialog}
  oncancel={(event) => { if (busy) event.preventDefault(); }}
>
  <div class="scan-header">
    <h2 id="clone-title">克隆仓库</h2>
    <p>在保存位置新建一个目录，克隆完成后加入 sgit。</p>
  </div>
  <form onsubmit={cloneRepository}>
    <div class="clone-fields">
      <label for="clone-address">仓库地址</label>
      <input id="clone-address" type="text" value={cloneAddress} oninput={updateCloneAddress}
        placeholder="https://github.com/owner/repo.git 或 git@github.com:owner/repo.git"
        autocomplete="off" spellcheck="false" required disabled={busy} />
      <label for="clone-parent">保存位置</label>
      <div class="clone-location">
        <input id="clone-parent" type="text" bind:value={cloneParent} placeholder="选择或输入目录的绝对路径"
          autocomplete="off" spellcheck="false" required disabled={busy} />
        <button class="secondary" type="button" onclick={chooseCloneParent} disabled={busy}>选择目录</button>
      </div>
      <label for="clone-name">新建目录名</label>
      <input id="clone-name" type="text" bind:value={cloneName} oninput={() => cloneNameEdited = true}
        autocomplete="off" spellcheck="false" required disabled={busy} />
    </div>
    {#if cloneError}<p class="banner error" role="alert">{cloneError}</p>{/if}
    <div class="scan-footer">
      <button class="secondary" type="button" onclick={() => cloneDialog.close()} disabled={busy}>取消</button>
      <button class="primary" type="submit" disabled={busy || !cloneAddress.trim() || !cloneParent.trim() || !cloneName.trim()}>
        {cloning ? '正在克隆…' : '克隆并添加'}
      </button>
    </div>
  </form>
</dialog>

<dialog
  class="scan-dialog"
  aria-labelledby="info-title"
  bind:this={infoDialog}
  oncancel={(event) => { if (busy) event.preventDefault(); }}
>
  {#if infoRow}
    {@const state = repositoryState(infoRow)}
    <div class="scan-header">
      <h2 id="info-title">{infoRow.repository.id}</h2>
      <p class="info-status" data-tone={state.tone}><span class="status-dot" aria-hidden="true"></span>{state.label}</p>
    </div>
    <dl class="repo-details">
      <dt>本地路径</dt>
      <dd><code>{infoRow.repository.path}</code></dd>
      <dt><label for="repository-group">分组</label></dt>
      <dd>
        <select id="repository-group" value={String(infoRow.repository.group_id ?? '')} disabled={busy}
          onchange={(event) => {
            if (!infoRow) return;
            const group = event.currentTarget.value;
            event.currentTarget.value = String(infoRow.repository.group_id ?? '');
            void moveToGroup(infoRow.repository, group ? Number(group) : null);
          }}>
          <option value="">未分组</option>
          {#each groups as group (group.id)}
            <option value={String(group.id)}>{group.name}</option>
          {/each}
        </select>
      </dd>
      <dt>当前分支</dt>
      <dd>{infoRow.branch ?? '未检出分支'}</dd>
      <dt>上游分支</dt>
      <dd>{infoRow.upstream ?? '未配置'}</dd>
      <dt>本地变更</dt>
      <dd>{infoRow.changes} 处已跟踪或暂存 · {infoRow.untracked} 项未跟踪</dd>
      {#if infoRow.ahead !== null && infoRow.behind !== null}
        <dt>远端进度</dt>
        <dd>{infoRow.ahead} 个待推送 · {infoRow.behind} 个待拉取 <span class="muted">（基于最近一次获取）</span></dd>
      {/if}
      <dt>远端地址</dt>
      <dd>
        {#if infoRemotes === null}
          {#if !infoError}<span class="muted" role="status">正在读取…</span>{/if}
        {:else if infoRemotes.length === 0}
          <span class="muted">未配置远端</span>
        {:else}
          <ul class="remote-list">
            {#each infoRemotes as remote (remote.name)}
              <li>
                <strong>{remote.name}</strong>
                {#each remote.fetch_urls as url}<code>{url}</code>{/each}
                {#if remote.push_urls.length}
                  <span class="muted">推送地址</span>
                  {#each remote.push_urls as url}<code>{url}</code>{/each}
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </dd>
    </dl>
    {#if infoRow.problem}<p class="banner warning">{infoRow.problem}</p>{/if}
    {#if infoError}<p class="banner error" role="alert">{infoError}</p>{/if}
    {#if confirmDiskRemoval}
      <div class="disk-confirmation">
        <strong>将整个仓库移到废纸篓或回收站？</strong>
        <code>{infoRow.repository.path}</code>
        <p>目录里的文件和本地提交也会一并移走。完成后，该仓库会从 sgit 列表移除。</p>
      </div>
      <div class="scan-footer">
        <button class="secondary" onclick={() => confirmDiskRemoval = false} disabled={busy}>取消删除</button>
        <button class="danger" onclick={() => infoRow && remove(infoRow.repository, true)} disabled={busy}>确认移到回收站</button>
      </div>
    {:else}
      <div class="scan-footer info-footer">
        <div class="actions">
          <button class="secondary" onclick={() => infoRow && remove(infoRow.repository)} disabled={busy}>从列表移除</button>
          <button class="secondary remove-repository" onclick={() => confirmDiskRemoval = true} disabled={busy}>从磁盘删除</button>
        </div>
        <button class="primary" onclick={() => infoDialog.close()} disabled={busy}>关闭</button>
      </div>
    {/if}
  {/if}
</dialog>

<dialog
  class="scan-dialog"
  aria-labelledby="scan-title"
  bind:this={previewDialog}
  oncancel={(event) => { if (busy) event.preventDefault(); }}
>
  {#if scanResult}
    <div class="scan-header">
      <h2 id="scan-title">扫描结果</h2>
      <p>找到 {scanResult.candidates.length} 个仓库。取消勾选你不想添加的仓库。</p>
    </div>
    {#if scanResult.skipped}
      <p class="banner warning" role="status">有 {scanResult.skipped} 项无法读取或验证，结果可能不完整。</p>
    {/if}
    {#if previewError}
      <p class="banner error" role="alert">{previewError}</p>
    {/if}
    {#if scanResult.candidates.length}
      <label class="scan-select-all">
        <input
          type="checkbox"
          checked={newCandidates.length > 0 && scanSelection.size === newCandidates.length}
          indeterminate={scanSelection.size > 0 && scanSelection.size < newCandidates.length}
          disabled={busy || newCandidates.length === 0}
          onchange={(event) => selectAllCandidates(event.currentTarget.checked)}
        />
        全选可添加的仓库
      </label>
      <ul class="scan-list">
        {#each scanResult.candidates as candidate (candidate.path)}
          <li>
            <label class="scan-row">
              <input
                type="checkbox"
                aria-label={`添加 ${candidate.name} · ${candidate.path}`}
                checked={scanSelection.has(candidate.path)}
                disabled={busy || candidate.registered}
                onchange={(event) => {
                  if (event.currentTarget.checked) scanSelection.add(candidate.path);
                  else scanSelection.delete(candidate.path);
                }}
              />
              <span class="scan-repo">
                <strong>{candidate.name}</strong>
                <span class="path" title={candidate.path}>{candidate.path}</span>
              </span>
              {#if candidate.registered}<span class="result queued">已添加</span>{/if}
            </label>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="scan-empty">所选目录中没有找到可同步的 Git 工作树。</p>
    {/if}
    <div class="scan-footer">
      <button class="secondary" onclick={() => previewDialog.close()} disabled={busy}>取消</button>
      <button class="primary" onclick={addSelected} disabled={busy || scanSelection.size === 0}>
        {busy ? '正在添加…' : `添加选中的 ${scanSelection.size} 个仓库`}
      </button>
    </div>
  {/if}
</dialog>

<dialog
  class="scan-dialog"
  aria-labelledby="sync-title"
  bind:this={syncDialog}
  oncancel={(event) => { if (syncChecking) event.preventDefault(); }}
>
  <div class="scan-header">
    <h2 id="sync-title">同步所选仓库</h2>
    <p>可以快进的更新会直接拉取。只有你确认后，sgit 才会提交和推送。</p>
  </div>
  {#if syncChecking}
    <p class="banner" role="status">正在同步或检查远端，请稍候…</p>
  {:else}
    {#if syncError}
      <p class="banner error" role="alert">{syncError}</p>
    {/if}
    {#if canConfirmSync && syncItems.some((item) => item.review?.untracked.length)}
      <p class="banner">勾选的新文件随本次提交推送，未勾选的文件留在本机。</p>
    {/if}
    <ul class="scan-list">
      {#each syncItems as { repository, review, problem } (repository.id)}
        <li class="sync-repo">
          <strong>{repository.id}</strong>
          <p class="path" title={repository.path}>{repository.path}</p>
          {#if problem}
            <p class="detail warning">{problem}</p>
          {:else if review}
            <p class="changes">{review.branch.replace(/^refs\/heads\//, '')} → {review.upstream} · {review.tracked} 处已跟踪或暂存变更 · {review.ahead} 个待推送提交</p>
            {#each review.untracked as file (file)}
              <label class="sync-file">
                <input
                  type="checkbox"
                  aria-label={`随 ${repository.id} 推送 ${file}`}
                  checked={syncSelection.has(syncFileKey(repository, file))}
                  disabled={!canConfirmSync || file.endsWith('/')}
                  onchange={(event) => {
                    const key = syncFileKey(repository, file);
                    if (event.currentTarget.checked) syncSelection.add(key);
                    else syncSelection.delete(key);
                  }}
                />
                <code>{file}</code>
              </label>
            {/each}
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
  <div class="scan-footer">
    <button class="secondary" onclick={() => syncDialog.close()} disabled={syncChecking}>关闭</button>
    <button class="secondary" onclick={() => checkSync()} disabled={syncChecking}>重新检查</button>
    {#if canConfirmSync}
      <button class="primary" onclick={confirmSync} disabled={syncChecking}>{willPush ? '确认并推送' : '确认保留在本机'}</button>
    {/if}
  </div>
</dialog>
