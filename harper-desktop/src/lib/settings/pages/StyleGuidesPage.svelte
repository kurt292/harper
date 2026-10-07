<script lang="ts">
import { Button, IconButton, Panel, Textarea, Toggle, TrashIcon } from 'components';
import { onMount } from 'svelte';
import {
	type StyleCheckReport,
	type StyleGuideView,
	type StyleModelStatus,
	StyleGuides,
} from '$lib/client';

let guides: StyleGuideView[] = [];
let guidesError = '';
let isGuidesLoading = true;
let isGuidesSaving = false;

let editingId: string | null = null;
let editorJson = '';
let editorError = '';
let editorNotice = '';

let modelStatus: StyleModelStatus | null = null;
let isModelStatusLoading = true;

let checkText = '';
let isChecking = false;
let checkError = '';
let report: StyleCheckReport | null = null;
let showDropped = false;

const NEW_GUIDE_TEMPLATE = {
	id: 'my-guide',
	name: 'My Guide',
	active: false,
	priority: 50,
	rules: [
		{ type: 'forbid_term', terms: ['utilize'], prefer: 'use' },
		{ type: 'forbid_pattern', regex: '\\b(very|really)\\b', ignore_case: true },
		{ type: 'max_sentence_words', value: 25 },
		{ type: 'voice', value: 'active' },
		{ type: 'freeform', instruction: 'Describe the tone you want here.' },
	],
};

onMount(() => {
	void loadGuides();
	void loadModelStatus();

	const refresh = () => {
		if (!isGuidesSaving && editingId === null) {
			void loadGuides();
		}
	};
	window.addEventListener('focus', refresh);
	return () => window.removeEventListener('focus', refresh);
});

async function loadGuides() {
	isGuidesLoading = true;
	guidesError = '';
	try {
		guides = await StyleGuides.list();
	} catch (error) {
		guidesError = `Unable to load style guides: ${error}`;
	} finally {
		isGuidesLoading = false;
	}
}

async function loadModelStatus() {
	isModelStatusLoading = true;
	try {
		modelStatus = await StyleGuides.modelStatus();
	} catch (error) {
		modelStatus = {
			endpoint: '',
			model: '',
			reachable: false,
			model_available: false,
			available_models: [],
			error: String(error),
		};
	} finally {
		isModelStatusLoading = false;
	}
}

async function setActive(guide: StyleGuideView, active: boolean) {
	isGuidesSaving = true;
	guidesError = '';
	try {
		await StyleGuides.setActive(guide.id, active);
		await loadGuides();
	} catch (error) {
		guidesError = `Unable to update “${guide.name}”: ${error}`;
	} finally {
		isGuidesSaving = false;
	}
}

function startEditing(guide: StyleGuideView) {
	editingId = guide.id;
	editorJson = guide.json;
	editorError = '';
	editorNotice = '';
}

function startNew() {
	editingId = '';
	editorJson = JSON.stringify(NEW_GUIDE_TEMPLATE, null, 2);
	editorError = '';
	editorNotice = '';
}

function cancelEditing() {
	editingId = null;
	editorJson = '';
	editorError = '';
	editorNotice = '';
}

async function saveEditor() {
	isGuidesSaving = true;
	editorError = '';
	editorNotice = '';
	try {
		editorJson = await StyleGuides.save(editorJson);
		editorNotice = 'Saved. The highlighter picks it up within a second.';
		await loadGuides();
		editingId = (JSON.parse(editorJson) as { id: string }).id;
	} catch (error) {
		editorError = String(error);
	} finally {
		isGuidesSaving = false;
	}
}

async function deleteGuide(guide: StyleGuideView) {
	if (!window.confirm(`Delete the style guide “${guide.name}”? This removes its file.`)) {
		return;
	}
	isGuidesSaving = true;
	guidesError = '';
	try {
		await StyleGuides.delete(guide.id);
		if (editingId === guide.id) {
			cancelEditing();
		}
		await loadGuides();
	} catch (error) {
		guidesError = `Unable to delete “${guide.name}”: ${error}`;
	} finally {
		isGuidesSaving = false;
	}
}

async function runCheck() {
	if (!checkText.trim()) {
		checkError = 'Paste or type some text first.';
		return;
	}
	isChecking = true;
	checkError = '';
	report = null;
	showDropped = false;
	try {
		report = await StyleGuides.check(checkText);
	} catch (error) {
		checkError = String(error);
	} finally {
		isChecking = false;
	}
}

function applySuggestion(index: number) {
	if (!report) return;
	const violation = report.violations[index];
	if (!violation.suggestion) return;
	const chars = Array.from(checkText);
	const [start, end] = violation.span;
	const current = chars.slice(start, end).join('');
	if (current !== violation.original) {
		checkError = 'The text changed since this check ran. Run the check again.';
		return;
	}
	chars.splice(start, end - start, ...Array.from(violation.suggestion));
	checkText = chars.join('');
	// Spans after this edit are stale; drop the applied finding and shift the rest.
	const delta = Array.from(violation.suggestion).length - (end - start);
	report = {
		...report,
		violations: report.violations
			.filter((_, i) => i !== index)
			.map((v) =>
				v.span[0] >= end ? { ...v, span: [v.span[0] + delta, v.span[1] + delta] } : v,
			),
	};
}

$: modelReady = modelStatus?.reachable && modelStatus?.model_available;
$: activeModelRuleCount = guides
	.filter((g) => g.active)
	.reduce((sum, g) => sum + g.model_rule_count, 0);
</script>

<div class="stanza">
  <div class="eyebrow">Style guides</div>
  <p class="section-copy">
    Named rule sets stored as JSON. Active guides add their deterministic rules to the
    highlighter alongside Harper's grammar rules. When two active guides conflict, the one
    with the lower priority number wins.
  </p>

  {#if guidesError}
    <p class="result-summary" role="alert">{guidesError}</p>
  {/if}

  <Panel>
    {#if isGuidesLoading && guides.length === 0}
      <div class="empty">Loading style guides...</div>
    {:else if guides.length === 0}
      <div class="empty">No style guides yet.</div>
    {:else}
      {#each guides as guide (guide.id)}
        <div class="app-row">
          <div class="grow">
            <strong>{guide.name}</strong>
            <p>
              {guide.rule_count} rules ({guide.model_rule_count} need the model) · priority {guide.priority}
              · <code>{guide.id}.json</code>
            </p>
          </div>
          <Button
            unstyled
            class="button"
            type="button"
            disabled={isGuidesSaving}
            on:click={() => startEditing(guide)}
          >Edit</Button>
          <IconButton
            danger
            disabled={isGuidesSaving}
            aria-label={`Delete ${guide.name}`}
            on:click={() => deleteGuide(guide)}
          >
            <TrashIcon className="control-icon" />
          </IconButton>
          <Toggle
            appearance="settings"
            checked={guide.active}
            disabled={isGuidesSaving}
            aria-label={`Toggle ${guide.name}`}
            on:click={() => setActive(guide, !guide.active)}
          />
        </div>
      {/each}
    {/if}
  </Panel>

  <div class="actions-row">
    <Button unstyled class="button" type="button" disabled={isGuidesSaving} on:click={startNew}
      >New guide...</Button
    >
    <span class="muted">Guides live in the style-guides folder next to Harper's config.</span>
  </div>

  {#if editingId !== null}
    <div class="editor">
      <div class="eyebrow">{editingId === '' ? 'New guide' : `Editing ${editingId}`}</div>
      <p class="section-copy">
        Rule types: <code>forbid_term</code>, <code>forbid_pattern</code>,
        <code>max_sentence_words</code> run instantly. <code>voice</code>,
        <code>reading_level</code> and <code>freeform</code> run only in the model check below.
      </p>
      <Textarea bind:value={editorJson} rows={18} spellcheck={false} className="json-editor" />
      {#if editorError}
        <p class="result-summary" role="alert">{editorError}</p>
      {/if}
      {#if editorNotice}
        <p class="result-summary">{editorNotice}</p>
      {/if}
      <div class="actions-row">
        <Button unstyled class="button" type="button" disabled={isGuidesSaving} on:click={saveEditor}
          >Save</Button
        >
        <Button unstyled class="button" type="button" disabled={isGuidesSaving} on:click={cancelEditing}
          >Close</Button
        >
      </div>
    </div>
  {/if}
</div>

<div class="divider"></div>

<div class="stanza">
  <div class="eyebrow">Style check with the local model</div>
  <p class="section-copy">
    Runs the active guides' model rules (voice, reading level, freeform instructions) against a
    draft. This takes several seconds on this machine, so it is a button, not a background pass.
    Nothing leaves the computer.
  </p>

  {#if isModelStatusLoading}
    <p class="result-summary">Checking the model server...</p>
  {:else if modelStatus && !modelStatus.reachable}
    <p class="result-summary" role="alert">
      Ollama is not reachable at {modelStatus.endpoint}. {modelStatus.error ?? ''}
    </p>
  {:else if modelStatus && !modelStatus.model_available}
    <p class="result-summary" role="alert">
      Ollama is running but the model <code>{modelStatus.model}</code> is not installed.
      Run <code>ollama pull {modelStatus.model}</code>.
      {#if modelStatus.available_models.length > 0}
        Installed: {modelStatus.available_models.join(', ')}.
      {/if}
    </p>
  {:else if modelStatus}
    <p class="result-summary">
      Model <code>{modelStatus.model}</code> ready at {modelStatus.endpoint}.
      {#if activeModelRuleCount === 0}
        No active guide has model rules, so there is nothing for it to check yet.
      {/if}
    </p>
  {/if}

  <Textarea
    bind:value={checkText}
    rows={8}
    placeholder="Paste a draft here, then press Check."
    className="check-input"
  />

  <div class="actions-row">
    <Button
      unstyled
      class="button"
      type="button"
      disabled={isChecking || !modelReady || activeModelRuleCount === 0}
      on:click={runCheck}
    >{isChecking ? 'Checking...' : 'Check with model'}</Button>
    <Button unstyled class="button" type="button" on:click={loadModelStatus}>Recheck server</Button>
    {#if report}
      <span class="muted">
        {report.violations.length} finding{report.violations.length === 1 ? '' : 's'} from
        {report.model} in {(report.elapsed_ms / 1000).toFixed(1)} s
        · guides: {report.guides.join(', ')}
      </span>
    {/if}
  </div>

  {#if checkError}
    <p class="result-summary" role="alert">{checkError}</p>
  {/if}

  {#if report}
    <Panel>
      {#if report.violations.length === 0}
        <div class="empty">No style violations the model was confident about.</div>
      {:else}
        {#each report.violations as violation, index}
          <div class="app-row finding">
            <div class="grow">
              <strong>{violation.rule}</strong>
              <span class="muted"> · confidence {Math.round(violation.confidence * 100)}%</span>
              <p class="quote">“{violation.original}”</p>
              {#if violation.suggestion}
                <p class="suggestion">→ “{violation.suggestion}”</p>
              {/if}
              {#if violation.explanation}
                <p>{violation.explanation}</p>
              {/if}
            </div>
            {#if violation.suggestion}
              <Button unstyled class="button" type="button" on:click={() => applySuggestion(index)}
                >Apply</Button
              >
            {/if}
          </div>
        {/each}
      {/if}
    </Panel>
    {#if report.dropped.length > 0}
      <div class="actions-row">
        <Button unstyled class="button" type="button" on:click={() => (showDropped = !showDropped)}
          >{showDropped ? 'Hide' : 'Show'} {report.dropped.length} dropped finding{report.dropped.length === 1 ? '' : 's'}</Button
        >
      </div>
      {#if showDropped}
        <ul class="dropped">
          {#each report.dropped as reason}
            <li class="muted">{reason}</li>
          {/each}
        </ul>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .editor {
    margin-top: 1rem;
  }

  :global(.json-editor),
  :global(.check-input) {
    width: 100%;
    font-size: 0.85rem;
    line-height: 1.4;
    margin-top: 0.5rem;
  }

  :global(.json-editor) {
    font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  }

  .finding p {
    margin: 0.15rem 0;
  }

  .quote {
    font-style: italic;
  }

  .suggestion {
    font-weight: 600;
  }

  .dropped {
    margin: 0.5rem 0 0 1rem;
    font-size: 0.85rem;
  }
</style>
