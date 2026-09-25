<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api } from "../api";
  import type { AppSettings, LocalModelProgress, LocalModelStatus, LocalPipelineStatus } from "../types";
  import type { UpdateStatus } from "../updater";
  import { applyTheme, type Theme } from "../utils";

  let {
    onBack,
    onCheckUpdates,
    updateStatus,
  }: {
    onBack: () => void;
    onCheckUpdates: () => void;
    updateStatus: UpdateStatus;
  } = $props();

  const checkingUpdate = $derived(
    updateStatus.kind === "checking" || updateStatus.kind === "downloading",
  );

  let settings = $state<AppSettings>({
    downloadDir: "",
    folderStructure: "by_date",
    customPrefix: "PlaudRecordings",
    filenameStyle: "clean",
    createInfoTxt: true,
    downloadTranscript: true,
    autoSync: false,
    autoSyncMinutes: 15,
    theme: "system",
    startMinimized: false,
    localTranscription: true,
    autoTranscribe: true,
    transcriptionModel: "parakeet-tdt-0.6b-v3-int8",
  });
  let autostart = $state(false);
  let modelStatuses = $state<LocalModelStatus[]>([]);
  let pipelineStatus = $state<LocalPipelineStatus | null>(null);
  let modelProgress = $state<LocalModelProgress | null>(null);
  // The model whose combined download is running (drives the per-row
  // "Downloading" state). `busyModelId` is any model operation (download or
  // delete) — it disables the buttons.
  let downloadingModelId = $state("");
  let busyModelId = $state("");
  const downloading = $derived(!!downloadingModelId);
  const busy = $derived(!!busyModelId);

  const selectedModel = $derived(
    modelStatuses.find((m) => m.id === settings.transcriptionModel) ?? null,
  );
  // Local transcription is one feature: the chosen ASR model plus the shared
  // speaker-label models. Both install together, so "installed" means both.
  function modelInstalled(model: LocalModelStatus): boolean {
    return model.installed && !!pipelineStatus?.installed;
  }
  const modelsInstalled = $derived(
    !!selectedModel && modelInstalled(selectedModel),
  );
  const combinedSizeMb = $derived(
    (selectedModel?.sizeMb ?? modelStatuses[0]?.sizeMb ?? 670) + (pipelineStatus?.sizeMb ?? 104),
  );

  function setTheme(theme: Theme) {
    settings.theme = theme;
    applyTheme(theme); // apply immediately for live preview
  }

  async function toggleAutostart(enabled: boolean) {
    autostart = enabled;
    try {
      await api.setAutostart(enabled);
    } catch (e) {
      error = String(e);
      autostart = !enabled; // revert on failure
    }
  }
  let example = $state("");
  let saving = $state(false);
  let saved = $state(false);
  let error = $state("");

  const structureOptions = [
    {
      id: "by_date",
      title: "By Date (recommended)",
      description: "Organize recordings into date folders.",
    },
    {
      id: "flat",
      title: "Flat",
      description: "Keep all recordings in one folder.",
    },
    {
      id: "by_date_device",
      title: "By Date + Device",
      description: "Add a device subfolder when available.",
    },
    {
      id: "custom_prefix",
      title: "Custom prefix",
      description: "Use your own top-level folder name.",
    },
  ] as const;

  $effect(() => {
    void refreshExample(settings);
  });

  async function load() {
    settings = await api.getSettings();
    await refreshExample(settings);
    try {
      modelStatuses = await api.listLocalModels();
    } catch {
      // The model status is non-critical to the rest of Settings.
    }
    try {
      pipelineStatus = await api.getLocalPipelineStatus();
    } catch {
      // Optional speech-processing models are non-critical to Settings.
    }
    try {
      autostart = await api.getAutostart();
    } catch {
      // ignore
    }
  }

  async function refreshExample(current: AppSettings) {
    example = await api.getPathExample(current);
  }

  async function pickFolder() {
    const folder = await api.pickDownloadFolder();
    if (folder) settings.downloadDir = folder;
  }

  async function save() {
    saving = true;
    saved = false;
    error = "";
    try {
      await api.saveSettings(settings);
      saved = true;
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  // Download one transcription model and the shared speaker-diarization models
  // together — local transcription is one feature, not two optional pieces.
  // Only the pieces that aren't already installed are fetched.
  async function downloadModels(modelId: string) {
    busyModelId = modelId;
    downloadingModelId = modelId;
    modelProgress = null;
    error = "";
    try {
      const model = modelStatuses.find((m) => m.id === modelId);
      if (model && !model.installed) {
        const updated = await api.downloadLocalModel(modelId);
        modelStatuses = modelStatuses.map((m) => (m.id === modelId ? updated : m));
        modelProgress = null;
      }
      if (!pipelineStatus?.installed) {
        pipelineStatus = await api.downloadLocalPipeline();
      }
    } catch (e) {
      const message = String(e);
      if (!message.toLowerCase().includes("cancel")) error = message;
    } finally {
      downloadingModelId = "";
      modelProgress = null;
      busyModelId = "";
    }
  }

  async function deleteModels(modelId: string) {
    busyModelId = modelId;
    error = "";
    try {
      await api.deleteLocalPipeline();
      await api.deleteLocalModel(modelId);
      pipelineStatus = await api.getLocalPipelineStatus();
      modelStatuses = await api.listLocalModels();
    } catch (e) {
      error = String(e);
    } finally {
      busyModelId = "";
    }
  }

  async function cancelModelDownload() {
    try {
      await api.cancelLocalModelDownload();
    } catch (e) {
      error = String(e);
    }
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1_000_000) return `${Math.round(bytes / 1_000)} KB`;
    return `${(bytes / 1_000_000).toFixed(bytes >= 1_000_000_000 ? 1 : 0)} MB`;
  }

  onMount(() => {
    const unlisten = listen<LocalModelProgress>("local-model-progress", (event) => {
      modelProgress = event.payload;
    });
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  });

  load();
</script>

<div class="card">
  <div class="settings-header">
    <div>
      <h2>Settings</h2>
      <p class="subtitle">Choose how recordings are saved on your computer.</p>
    </div>
    <button class="btn btn-secondary" onclick={onBack}>Back</button>
  </div>

  {#if error}
    <div class="status error">{error}</div>
  {:else if saved}
    <div class="status success">Settings saved.</div>
  {/if}

  <fieldset class="field">
    <legend>Updates</legend>
    <div class="toggle-row">
      <div>
        <strong>App updates</strong>
        <div class="meta">
          Plaud Sync checks for new versions on launch and installs them with your approval.
          {#if updateStatus.kind === "available"}
            Version {updateStatus.version} is available.
          {:else if updateStatus.kind === "downloading"}
            Downloading… {updateStatus.percent}%
          {:else if updateStatus.kind === "ready"}
            Update installed — restart to finish.
          {:else if updateStatus.kind === "uptodate"}
            You're on the latest version.
          {:else if updateStatus.kind === "error"}
            Last check failed: {updateStatus.message}
          {/if}
        </div>
      </div>
      <button class="btn btn-secondary btn-sm" onclick={onCheckUpdates} disabled={checkingUpdate}>
        {checkingUpdate ? "Checking…" : "Check for updates"}
      </button>
    </div>
  </fieldset>

  <div class="field">
    <label for="download-dir">Save location</label>
    <div class="folder-row">
      <input id="download-dir" type="text" bind:value={settings.downloadDir} readonly />
      <button class="btn btn-secondary" onclick={pickFolder}>Choose</button>
      <button class="btn btn-ghost" onclick={() => api.openDownloadFolder()}>Open</button>
    </div>
  </div>

  <fieldset class="field model-field">
    <legend>Local transcription</legend>
    <div class="toggle-row">
      <div>
        <strong>Enable local transcription</strong>
        <div class="meta">
          Transcribe downloaded recordings on this computer and label who is speaking
          (Speaker 1, Speaker 2) using on-device voice-activity detection and speaker
          diarization. Audio stays local and it works on macOS and Windows. Choose a
          transcription model below (about {combinedSizeMb} MB for the selected model);
          model revisions are pinned and change only through a Plaud Sync update.
        </div>
      </div>
      <input type="checkbox" bind:checked={settings.localTranscription} />
    </div>
    {#if settings.localTranscription}
      <div class="toggle-row">
        <div>
          <strong>Auto-transcribe new downloads</strong>
          <div class="meta">
            Automatically transcribe recordings as they download (including during
            auto-sync), so transcripts are ready without a click. Uses your CPU for a few
            minutes per recording; the models download once if they aren't installed yet.
          </div>
        </div>
        <input type="checkbox" bind:checked={settings.autoTranscribe} />
      </div>
    {/if}
    {#if modelStatuses.length}
      <div class="model-picker">
        {#each modelStatuses as model}
          <!-- Clicking the card selects the model; it is only usable once installed. -->
          <div
            class="model-card {settings.transcriptionModel === model.id ? "selected" : ""}"
            role="button"
            tabindex="0"
            onclick={() => (settings.transcriptionModel = model.id)}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") settings.transcriptionModel = model.id;
            }}
          >
            <input
              type="radio"
              name="transcription-model"
              checked={settings.transcriptionModel === model.id}
              aria-label={`Use {model.name}`}
            />
            <div class="model-info">
              <strong>{model.name}{#if model.isDefault} · default{/if}</strong>
              <div class="meta">{model.description}</div>
              <div class="meta">
                About {model.sizeMb} MB
                {#if !modelInstalled(model)}+ {pipelineStatus?.sizeMb ?? 104} MB speaker labels{/if}
              </div>
            </div>
            <div class="model-state">
              {#if downloadingModelId === model.id}
                <span class="status-pill downloading">Downloading</span>
              {:else if modelInstalled(model)}
                <span class="status-pill installed">Installed</span>
              {:else if model.installed}
                <span class="status-pill">Speech model only</span>
              {:else}
                <span class="status-pill">Not installed</span>
              {/if}
            </div>
            <div class="model-actions">
              {#if downloadingModelId === model.id}
                <button class="btn btn-ghost btn-sm" onclick={() => cancelModelDownload()}>Cancel</button>
              {:else if modelInstalled(model)}
                <button class="btn btn-ghost btn-sm" onclick={() => deleteModels(model.id)} disabled={busy}>
                  Remove models
                </button>
              {:else}
                <button class="btn btn-primary btn-sm" onclick={() => downloadModels(model.id)} disabled={busy}>
                  {model.installed ? "Download speech models" : "Download models"}
                </button>
              {/if}
            </div>
          </div>
          {#if downloadingModelId === model.id}
            <div class="progress-wrap">
              <div class="progress-bar"><div style={`width: ${modelProgress ? (modelProgress.downloadedTotal / Math.max(modelProgress.total, 1)) * 100 : 0}%`}></div></div>
              <span class="meta">{#if modelProgress}{modelProgress.file} · {formatBytes(modelProgress.downloadedTotal)} / {formatBytes(modelProgress.total)}{:else}Preparing download…{/if}</span>
            </div>
          {/if}
        {/each}
      </div>
      {#if modelsInstalled}
        <p class="meta auto-note">
          Ready for local transcription with speaker labels · revision {selectedModel?.revision.slice(0, 8)}
        </p>
      {:else}
        <p class="meta auto-note">
          Download the models for the transcription model you want to use; they are fetched once.
        </p>
      {/if}
    {/if}
  </fieldset>

  <div class="settings-grid">
    <div class="col">
      <fieldset class="field">
        <legend>Folder structure</legend>
        <div class="option-grid">
          {#each structureOptions as option}
            <button
              class="option-card {settings.folderStructure === option.id ? 'selected' : ''}"
              onclick={() => (settings.folderStructure = option.id)}
            >
              <strong>{option.title}</strong>
              <div class="meta">{option.description}</div>
            </button>
          {/each}
        </div>
      </fieldset>

      {#if settings.folderStructure === "custom_prefix"}
        <div class="field">
          <label for="prefix">Custom folder name</label>
          <input id="prefix" bind:value={settings.customPrefix} placeholder="WorkMeetings" />
        </div>
      {/if}
    </div>

    <div class="col">
      <fieldset class="field">
        <legend>Filename style</legend>
        <div class="segmented">
          <button
            class:active={settings.filenameStyle === "clean"}
            onclick={() => (settings.filenameStyle = "clean")}
            title={'Spaces and symbols become hyphens.\ne.g. “Team Standup, June” → Team-Standup-June.mp3'}
          >
            Clean names
          </button>
          <button
            class:active={settings.filenameStyle === "original"}
            onclick={() => (settings.filenameStyle = "original")}
            title={'Keeps Plaud’s original title (only illegal characters removed).\ne.g. “Team Standup, June” → Team Standup, June.mp3'}
          >
            Original Plaud names
          </button>
        </div>
      </fieldset>

      <div class="toggle-row">
        <div>
          <strong>Download transcript</strong>
          <div class="meta">Save a .txt file when a transcript exists in Plaud.</div>
        </div>
        <input type="checkbox" bind:checked={settings.downloadTranscript} />
      </div>

      <div class="toggle-row">
        <div>
          <strong>Create info .txt file</strong>
          <div class="meta">Include date, duration, and basic details for each recording.</div>
        </div>
        <input type="checkbox" bind:checked={settings.createInfoTxt} />
      </div>

      <div class="toggle-row">
        <div>
          <strong>Auto-sync new recordings</strong>
          <div class="meta">Check Plaud in the background and download new recordings automatically.</div>
        </div>
        <input type="checkbox" bind:checked={settings.autoSync} />
      </div>

      {#if settings.autoSync}
        <p class="meta auto-note">
          New recordings download automatically, usually within a minute of appearing in your
          Plaud account.
        </p>
      {/if}

      <div class="toggle-row">
        <div>
          <strong>Start at login</strong>
          <div class="meta">Launch Plaud Sync automatically when you sign in (macOS &amp; Windows).</div>
        </div>
        <input
          type="checkbox"
          checked={autostart}
          onchange={(e) => toggleAutostart(e.currentTarget.checked)}
        />
      </div>

      <div class="toggle-row">
        <div>
          <strong>Start minimized</strong>
          <div class="meta">Open minimized so it runs quietly as a background sync tool.</div>
        </div>
        <input type="checkbox" bind:checked={settings.startMinimized} />
      </div>
    </div>
  </div>

  <fieldset class="field">
    <legend>Appearance</legend>
    <div class="segmented">
      <button class:active={settings.theme === "system"} onclick={() => setTheme("system")}>
        System
      </button>
      <button class:active={settings.theme === "light"} onclick={() => setTheme("light")}>
        Light
      </button>
      <button class:active={settings.theme === "dark"} onclick={() => setTheme("dark")}>
        Dark
      </button>
    </div>
  </fieldset>

  <div class="example-box">
    <strong>Example path</strong>
    <code>{example}</code>
  </div>

  <button class="btn btn-primary btn-large" onclick={save} disabled={saving}>
    {saving ? "Saving..." : "Save Settings"}
  </button>
</div>

<style>
  .settings-header {
    display: flex;
    justify-content: space-between;
    gap: 16px;
    align-items: flex-start;
    margin-bottom: 8px;
  }

  .settings-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px 20px;
    align-items: start;
  }

  .settings-grid .col {
    min-width: 0;
  }

  @media (max-width: 620px) {
    .settings-grid {
      grid-template-columns: 1fr;
    }
  }

  .folder-row {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 8px;
  }

  .option-card {
    text-align: left;
    width: 100%;
    cursor: pointer;
  }

  fieldset {
    border: none;
    padding: 0;
    margin: 0 0 16px;
  }

  legend {
    font-size: 0.9rem;
    font-weight: 600;
    margin-bottom: 6px;
  }

  .meta {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin-top: 4px;
  }

  .example-box {
    margin: 14px 0;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--surface-muted);
  }

  .example-box code {
    display: block;
    margin-top: 8px;
    word-break: break-all;
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .model-field {
    margin-top: 4px;
  }

  .model-picker {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 10px;
  }

  .model-card {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--surface-muted);
    border: 1px solid transparent;
    cursor: pointer;
    text-align: left;
  }

  .model-card.selected {
    border-color: var(--primary);
  }

  .model-card input[type="radio"] {
    flex: none;
    pointer-events: none;
    margin: 0;
  }

  .model-info {
    flex: 1;
    min-width: 0;
  }

  .model-state {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .status-pill {
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    white-space: nowrap;
  }

  .status-pill.installed { color: var(--success); }
  .status-pill.downloading { color: var(--primary); }

  .model-actions { flex: none; }

  .progress-wrap { margin-top: 10px; }
</style>
