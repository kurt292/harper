<script lang="ts">
import { Button, IconButton, Input, Panel, Select, Textarea, Toggle, TrashIcon } from 'components';
import { onMount } from 'svelte';
import {
	type StyleCheckReport,
	type StyleGuideView,
	type StyleModelStatus,
	StyleGuides,
} from '$lib/client';

// ---------------------------------------------------------------------------
// Guide document model (mirrors broadside-style's JSON format)
// ---------------------------------------------------------------------------

type RuleType =
	| 'forbid_term'
	| 'forbid_pattern'
	| 'max_sentence_words'
	| 'voice'
	| 'reading_level'
	| 'freeform';

/** One rule as edited in the form. `terms` is kept as text so typing commas feels natural. */
interface RuleDraft {
	type: RuleType;
	terms: string;
	prefer: string;
	message: string;
	regex: string;
	ignore_case: boolean;
	value: number;
	voice: string;
	max_grade: number;
	instruction: string;
}

interface GuideDraft {
	id: string;
	name: string;
	active: boolean;
	priority: number;
	apps: string;
	urls: string;
	rules: RuleDraft[];
}

const RULE_TYPES: { value: RuleType; name: string }[] = [
	{ value: 'forbid_term', name: 'Forbid term (instant)' },
	{ value: 'forbid_pattern', name: 'Forbid pattern, regex (instant)' },
	{ value: 'max_sentence_words', name: 'Max words per sentence (instant)' },
	{ value: 'voice', name: 'Voice (model check)' },
	{ value: 'reading_level', name: 'Reading level (model check)' },
	{ value: 'freeform', name: 'Freeform instruction (model check)' },
];

function emptyRule(type: RuleType): RuleDraft {
	return {
		type,
		terms: '',
		prefer: '',
		message: '',
		regex: '',
		ignore_case: true,
		value: 25,
		voice: 'active',
		max_grade: 9,
		instruction: '',
	};
}

function splitList(text: string): string[] {
	return text
		.split(',')
		.map((s) => s.trim())
		.filter((s) => s.length > 0);
}

/** Parses a guide's JSON (as stored) into the form model. Unknown fields are dropped. */
function draftFromJson(json: string): GuideDraft {
	const doc = JSON.parse(json) as Record<string, unknown>;
	const bindings = (doc.bindings ?? {}) as { apps?: string[]; urls?: string[] };
	const rules = ((doc.rules ?? []) as Record<string, unknown>[]).map((r) => {
		const rule = emptyRule((r.type as RuleType) ?? 'forbid_term');
		rule.terms = ((r.terms as string[]) ?? []).join(', ');
		rule.prefer = (r.prefer as string) ?? '';
		rule.message = (r.message as string) ?? '';
		rule.regex = (r.regex as string) ?? '';
		rule.ignore_case = (r.ignore_case as boolean) ?? true;
		if (rule.type === 'max_sentence_words') rule.value = Number(r.value ?? 25);
		if (rule.type === 'voice') rule.voice = (r.value as string) ?? 'active';
		rule.max_grade = Number(r.max_grade ?? 9);
		rule.instruction = (r.instruction as string) ?? '';
		return rule;
	});
	return {
		id: (doc.id as string) ?? '',
		name: (doc.name as string) ?? '',
		active: Boolean(doc.active),
		priority: Number(doc.priority ?? 100),
		apps: (bindings.apps ?? []).join(', '),
		urls: (bindings.urls ?? []).join(', '),
		rules,
	};
}

/** Serializes the form model to the JSON the store validates and saves. */
function jsonFromDraft(draft: GuideDraft): string {
	const rules = draft.rules.map((r) => {
		switch (r.type) {
			case 'forbid_term':
				return {
					type: r.type,
					terms: splitList(r.terms),
					...(r.prefer.trim() ? { prefer: r.prefer.trim() } : {}),
					...(r.message.trim() ? { message: r.message.trim() } : {}),
				};
			case 'forbid_pattern':
				return {
					type: r.type,
					regex: r.regex,
					ignore_case: r.ignore_case,
					...(r.prefer.trim() ? { prefer: r.prefer.trim() } : {}),
					...(r.message.trim() ? { message: r.message.trim() } : {}),
				};
			case 'max_sentence_words':
				return { type: r.type, value: Number(r.value) };
			case 'voice':
				return { type: r.type, value: r.voice.trim() };
			case 'reading_level':
				return { type: r.type, max_grade: Number(r.max_grade) };
			case 'freeform':
				return { type: r.type, instruction: r.instruction.trim() };
		}
	});
	const apps = splitList(draft.apps);
	const urls = splitList(draft.urls);
	const doc: Record<string, unknown> = {
		id: draft.id.trim(),
		name: draft.name.trim(),
		active: draft.active,
		priority: Number(draft.priority),
		rules,
	};
	if (apps.length || urls.length) {
		doc.bindings = { ...(apps.length ? { apps } : {}), ...(urls.length ? { urls } : {}) };
	}
	return JSON.stringify(doc, null, 2);
}

// ---------------------------------------------------------------------------
// Page state
// ---------------------------------------------------------------------------

let guides: StyleGuideView[] = [];
let guidesError = '';
let isGuidesLoading = true;
let isGuidesSaving = false;

let editingId: string | null = null;
let draft: GuideDraft | null = null;
let editorView: 'form' | 'json' = 'form';
let editorJson = '';
let editorError = '';
let editorNotice = '';
let newRuleType: RuleType = 'forbid_term';

let modelStatus: StyleModelStatus | null = null;
let isModelStatusLoading = true;

let checkText = '';
let isChecking = false;
let checkError = '';
let report: StyleCheckReport | null = null;
let showDropped = false;

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
	editorError = '';
	editorNotice = '';
	try {
		draft = draftFromJson(guide.json);
		editorJson = guide.json;
		editorView = 'form';
	} catch (error) {
		draft = null;
		editorJson = guide.json;
		editorView = 'json';
		editorError = `This guide could not be loaded into the form; edit it as JSON. ${error}`;
	}
}

function startNew() {
	editingId = '';
	editorError = '';
	editorNotice = '';
	draft = {
		id: 'my-guide',
		name: 'My Guide',
		active: false,
		priority: 50,
		apps: '',
		urls: '',
		rules: [emptyRule('forbid_term')],
	};
	editorJson = jsonFromDraft(draft);
	editorView = 'form';
}

function cancelEditing() {
	editingId = null;
	draft = null;
	editorJson = '';
	editorError = '';
	editorNotice = '';
}

function switchView(view: 'form' | 'json') {
	editorError = '';
	if (view === 'json' && draft) {
		editorJson = jsonFromDraft(draft);
	}
	if (view === 'form') {
		try {
			draft = draftFromJson(editorJson);
		} catch (error) {
			editorError = `The JSON is not valid yet: ${error}`;
			return;
		}
	}
	editorView = view;
}

function addRule() {
	if (!draft) return;
	draft.rules = [...draft.rules, emptyRule(newRuleType)];
}

function removeRule(index: number) {
	if (!draft) return;
	draft.rules = draft.rules.filter((_, i) => i !== index);
}

function changeRuleType(index: number, type: RuleType) {
	if (!draft) return;
	const fresh = emptyRule(type);
	// Keep what carries over between the two forbid rule kinds.
	fresh.prefer = draft.rules[index].prefer;
	fresh.message = draft.rules[index].message;
	draft.rules[index] = fresh;
	draft.rules = draft.rules;
}

async function saveEditor() {
	isGuidesSaving = true;
	editorError = '';
	editorNotice = '';
	try {
		const json = editorView === 'form' && draft ? jsonFromDraft(draft) : editorJson;
		editorJson = await StyleGuides.save(json);
		draft = draftFromJson(editorJson);
		editorNotice = 'Saved. The highlighter picks it up within a second.';
		await loadGuides();
		editingId = draft.id;
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
    Named rule sets stored as JSON. Active guides add their instant rules to the highlighter
    next to Harper's grammar rules; a guide bound to an app switches on by itself while that
    app has focus. When two active guides conflict, the lower priority number wins.
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
      <div class="editor-head">
        <div class="eyebrow">{editingId === '' ? 'New guide' : `Editing ${editingId}`}</div>
        <div class="view-switch">
          <Button unstyled class={`button ${editorView === 'form' ? 'selected' : ''}`} type="button" on:click={() => switchView('form')}>Form</Button>
          <Button unstyled class={`button ${editorView === 'json' ? 'selected' : ''}`} type="button" on:click={() => switchView('json')}>JSON</Button>
        </div>
      </div>

      {#if editorView === 'form' && draft}
        <div class="field-grid">
          <label class="field">
            <span>Name</span>
            <Input bind:value={draft.name} placeholder="Nimbus B2B Email" />
          </label>
          <label class="field">
            <span>Id (file name; lowercase, digits, - and _)</span>
            <Input bind:value={draft.id} placeholder="nimbus-b2b-email" disabled={editingId !== ''} />
          </label>
          <label class="field">
            <span>Priority (lower wins conflicts)</span>
            <Input type="number" bind:value={draft.priority} min="0" />
          </label>
          <label class="field">
            <span>Active</span>
            <Toggle appearance="settings" checked={draft.active} on:click={() => { if (draft) draft.active = !draft.active; }} aria-label="Guide active" />
          </label>
          <label class="field wide">
            <span>Switch on in these apps (comma separated executable names)</span>
            <Input bind:value={draft.apps} placeholder="outlook.exe, olk.exe" />
          </label>
          <label class="field wide">
            <span>Switch on at these sites (comma separated; not acted on yet)</span>
            <Input bind:value={draft.urls} placeholder="mail.google.com" />
          </label>
        </div>

        <div class="eyebrow rules-head">Rules</div>
        {#if draft.rules.length === 0}
          <p class="muted">No rules yet. Add one below.</p>
        {/if}
        {#each draft.rules as rule, index (index)}
          <div class="rule-row">
            <div class="rule-type">
              <Select
                size="sm"
                items={RULE_TYPES}
                value={rule.type}
                on:change={(event) => changeRuleType(index, (event.detail.currentTarget as HTMLSelectElement).value as RuleType)}
              />
            </div>
            <div class="rule-fields">
              {#if rule.type === 'forbid_term'}
                <label class="field"><span>Terms (comma separated; multi-word allowed)</span><Input size="sm" bind:value={rule.terms} placeholder="weed, pot, marijuana" /></label>
                <label class="field"><span>Prefer instead (optional)</span><Input size="sm" bind:value={rule.prefer} placeholder="cannabis" /></label>
                <label class="field wide"><span>Message shown on the card (optional)</span><Input size="sm" bind:value={rule.message} /></label>
              {:else if rule.type === 'forbid_pattern'}
                <label class="field"><span>Regular expression</span><Input size="sm" bind:value={rule.regex} placeholder={'\\b(very|really|just)\\b'} /></label>
                <label class="field check"><input type="checkbox" bind:checked={rule.ignore_case} /><span>Ignore case</span></label>
                <label class="field"><span>Prefer instead (optional)</span><Input size="sm" bind:value={rule.prefer} /></label>
                <label class="field wide"><span>Message shown on the card (optional)</span><Input size="sm" bind:value={rule.message} /></label>
              {:else if rule.type === 'max_sentence_words'}
                <label class="field"><span>Maximum words per sentence</span><Input size="sm" type="number" bind:value={rule.value} min="1" /></label>
              {:else if rule.type === 'voice'}
                <label class="field"><span>Required voice</span><Input size="sm" bind:value={rule.voice} placeholder="active" /></label>
              {:else if rule.type === 'reading_level'}
                <label class="field"><span>Maximum US grade level</span><Input size="sm" type="number" bind:value={rule.max_grade} min="1" max="16" /></label>
              {:else if rule.type === 'freeform'}
                <label class="field wide"><span>Instruction for the model</span><Textarea bind:value={rule.instruction} rows={2} className="rule-textarea" /></label>
              {/if}
            </div>
            <IconButton danger aria-label="Remove rule" on:click={() => removeRule(index)}>
              <TrashIcon className="control-icon" />
            </IconButton>
          </div>
        {/each}
        <div class="actions-row">
          <Select size="sm" items={RULE_TYPES} bind:value={newRuleType} />
          <Button unstyled class="button" type="button" on:click={addRule}>Add rule</Button>
        </div>
      {:else}
        <p class="section-copy">
          Rule types: <code>forbid_term</code>, <code>forbid_pattern</code>,
          <code>max_sentence_words</code> run instantly. <code>voice</code>,
          <code>reading_level</code> and <code>freeform</code> run only in the model check.
        </p>
        <Textarea bind:value={editorJson} rows={18} spellcheck={false} className="json-editor" />
      {/if}

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
    Nothing leaves the computer. In any text field, Ctrl+Alt+H runs the same check on what you
    are writing and shows the findings as underlines.
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

  .editor-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .view-switch {
    display: flex;
    gap: 0.25rem;
  }

  .view-switch :global(.button.selected) {
    font-weight: 600;
    text-decoration: underline;
  }

  .field-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.6rem 1rem;
    margin: 0.5rem 0 1rem;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.8rem;
  }

  .field > span {
    opacity: 0.75;
  }

  .field.wide {
    grid-column: 1 / -1;
  }

  .field.check {
    flex-direction: row;
    align-items: center;
    gap: 0.4rem;
    align-self: end;
    padding-bottom: 0.5rem;
  }

  .rules-head {
    margin-top: 0.5rem;
  }

  .rule-row {
    display: grid;
    grid-template-columns: 220px 1fr auto;
    gap: 0.75rem;
    align-items: start;
    padding: 0.6rem 0;
    border-bottom: 1px solid rgba(0, 0, 0, 0.08);
  }

  .rule-fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.4rem 0.75rem;
  }

  :global(.json-editor),
  :global(.check-input),
  :global(.rule-textarea) {
    width: 100%;
    font-size: 0.85rem;
    line-height: 1.4;
    margin-top: 0.25rem;
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
