import test from 'node:test';
import assert from 'node:assert/strict';
import { Readable } from 'node:stream';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { cleanupAdapter, failureOutput, sdkErrorCategory } from './runner.mjs';
import { OPTIONS, LIMITS, safeEnv, denyPermission, parseInput, parseAnswer, collector, runSession, readBounded } from './runner.mjs';
import { PROTOCOL_VERSION } from '@agentclientprotocol/sdk';
const answer = {packetId: 'p', summary: [{text: 'A claim', citations: [{path: 'src/a.rs', startLine: 1, endLine: 1, quote: 'fn a() {}'}]}], branches: [], limitations: []};
const input = {packetId: 'p', prompt: 'Untrusted evidence'};
const auth = state => state.auth({authStatus: {kind: 'account', account: {plan: 'max'}}});
const chunk = (state, text = JSON.stringify(answer)) => state.update({sessionId: 's', update: {sessionUpdate: 'agent_message_chunk', content: {type: 'text', text}}});
function mock(state, {stopReason = 'end_turn', model = 'sonnet', selected = 'sonnet', authenticate = true, promptError = false} = {}) {
  const calls = [];
  return {calls, async request(method, params) {
    calls.push([method, params]);
    if (method === 'initialize') return {protocolVersion: PROTOCOL_VERSION};
    if (method === 'session/new') {
      if (authenticate) auth(state);
      else state.auth({authStatus: {kind: 'none'}});
      return {sessionId: 's', configOptions: [{id: 'model', options: [{value: model}]}]};
    }
    if (method === 'session/set_config_option') return {configOptions: [{id: 'model', currentValue: selected}]};
    if (method === 'session/prompt') {
      const text = JSON.stringify(answer); chunk(state, text.slice(0, 30)); chunk(state, text.slice(30));
      if (promptError) throw new Error('secret key and partial response');
      return {stopReason};
    }
    throw new Error('unexpected request');
  }};
}
test('offline ACP handshake selects sonnet and uses no tools or capabilities', async () => {
  const state = collector(), agent = mock(state);
  assert.deepEqual(await runSession(agent, input, '/private/scratch', state), {answer, estimatedUsd: null});
  assert.deepEqual(agent.calls.map(c => c[0]), ['initialize', 'session/new', 'session/set_config_option', 'session/prompt']);
  assert.deepEqual(agent.calls[0][1].clientCapabilities, {fs: {readTextFile: false, writeTextFile: false}, terminal: false});
  const session = agent.calls[1][1];
  assert.deepEqual(session.mcpServers, []); assert.equal(session.cwd, '/private/scratch');
  assert.deepEqual(session._meta.claudeCode.options, OPTIONS);
  assert.equal(session._meta.claudeCode.options.strictMcpConfig, true);
  assert.deepEqual(session._meta.claudeCode.options.settings, {disableAllHooks: true});
  assert.deepEqual(OPTIONS.tools, []); assert.deepEqual(OPTIONS.allowedTools, []); assert.deepEqual(OPTIONS.settingSources, []);
  assert.equal(OPTIONS.persistSession, false); assert.equal(OPTIONS.maxTurns, 2); assert.equal(OPTIONS.maxBudgetUsd, 1);
  assert.equal(LIMITS.timeout, 120000);
});
test('environment is allowlisted, never inherited keys or provider config', () => {
  const source = {HOME: '/home', PATH: '/bin', JEV_KEY: 'secret', ANTHROPIC_API_KEY: 'secret', OPENAI_API_KEY: 'secret', AWS_ACCESS_KEY_ID: 'secret', NODE_OPTIONS: 'secret', CLAUDE_CODE_OAUTH_TOKEN: 'secret', ANTHROPIC_BASE_URL: 'secret', CLAUDE_CONFIG_DIR: 'secret'};
  assert.deepEqual(safeEnv(source), {HOME: '/home', PATH: '/bin', CLAUDE_CODE_MAX_OUTPUT_TOKENS: '4096'});
  assert.ok(!JSON.stringify(safeEnv(source)).includes('secret'));
});
test('every incomplete stop and errors after text fail', async () => {
  for (const stopReason of ['max_tokens', 'max_turn_requests', 'refusal', 'cancelled', 'error', undefined]) {
    const state = collector(); auth(state); state.begin('s'); chunk(state);
    assert.throws(() => state.finish({stopReason}, 'p'));
  }
  const state = collector(); await assert.rejects(runSession(mock(state, {promptError: true}), input, '/scratch', state));
});
test('missing auth and unavailable or mismatched model prevent prompt', async () => {
  for (const options of [{authenticate: false}, {model: 'opus'}, {selected: 'opus'}]) {
    const state = collector(), agent = mock(state, options);
    await assert.rejects(runSession(agent, input, '/scratch', state));
    assert.ok(!agent.calls.some(c => c[0] === 'session/prompt'));
  }
  for (const status of [{kind: 'api_key'}, {kind: 'gateway'}, {kind: 'none'}, {kind: 'account', account: {plan: 'api'}}]) {
    const state = collector(); state.auth({authStatus: status}); assert.throws(() => state.begin('s'));
  }
});
test('auth/model changes, permission requests, tool use and nontext invalidate output', () => {
  for (const mutate of [s => s.auth({authStatus: {kind: 'api_key'}}), s => s.permission(),
    s => s.update({sessionId: 's', update: {sessionUpdate: 'tool_call'}}),
    s => s.update({sessionId: 'other', update: {sessionUpdate: 'agent_message_chunk', content: {type: 'text', text: ''}}}),
    s => s.update({sessionId: 's', update: {sessionUpdate: 'current_model_update', currentModelId: 'opus'}}),
    s => s.update({sessionId: 's', update: {sessionUpdate: 'config_option_update', configOptions: [{id: 'model', currentValue: 'opus'}]}}),
    s => s.update({sessionId: 's', update: {sessionUpdate: 'agent_message_chunk', content: {type: 'image'}}})]) {
    const state = collector(); auth(state); state.begin('s'); chunk(state); mutate(state);
    assert.throws(() => state.finish({stopReason: 'end_turn'}, 'p'));
  }
  assert.deepEqual(denyPermission(), {outcome: {outcome: 'cancelled'}});
});
test('strict JSON schema rejects fences, prose, partial and foreign answers', () => {
  const valid = JSON.stringify(answer);
  for (const text of ['```json\n' + valid + '\n```', valid + ' prose', valid + valid, '{', JSON.stringify({...answer, extra: 1}), JSON.stringify({...answer, packetId: 'other'}), JSON.stringify({...answer, summary: []})]) assert.throws(() => parseAnswer(text, 'p'));
  const invalid = structuredClone(answer); invalid.summary[0].citations[0].endLine = 13;
  assert.throws(() => parseAnswer(JSON.stringify(invalid), 'p'));
  assert.deepEqual(parseAnswer(valid, 'p'), answer);
  for (const limitation of [' ', 'x'.repeat(1201)])
    assert.throws(() => parseAnswer(JSON.stringify({...answer, limitations: [limitation]}), 'p'));
  const blankQuote = structuredClone(answer); blankQuote.summary[0].citations[0].quote = ' \n ';
  assert.throws(() => parseAnswer(JSON.stringify(blankQuote), 'p'));
});
test('bounded input and answer collection', async () => {
  assert.deepEqual(parseInput(JSON.stringify(input)), input);
  assert.throws(() => parseInput(JSON.stringify({...input, model: 'opus'})));
  assert.throws(() => parseInput(JSON.stringify({...input, prompt: 'x'.repeat(LIMITS.prompt + 1)})));
  await assert.rejects(readBounded(Readable.from(['abc', 'def']), 5));
  const state = collector(); auth(state); state.begin('s'); chunk(state, 'x'.repeat(LIMITS.answer + 1));
  assert.throws(() => state.finish({stopReason: 'end_turn'}, 'p'));
});
test('usage cost is finite USD estimate only', () => {
  const state = collector(); auth(state); state.begin('s'); chunk(state);
  for (const cost of [{amount: .12, currency: 'USD'}, {amount: -1, currency: 'USD'}, {amount: 999, currency: 'EUR'}, {amount: Infinity, currency: 'USD'}]) state.update({sessionId: 's', update: {sessionUpdate: 'usage_update', cost}});
  assert.equal(state.finish({stopReason: 'end_turn'}, 'p').estimatedUsd, .12);
});

test('CLI invalid input emits no success and redacts secret-bearing errors', async () => {
  const child = spawn(process.execPath, ['runner.mjs'], {cwd: new URL('.', import.meta.url), stdio: ['pipe', 'pipe', 'pipe']});
  let out = '', err = '';
  child.stdout.on('data', x => { out += x; }); child.stderr.on('data', x => { err += x; });
  const exited = once(child, 'exit');
  child.stdin.end('secret-ANTHROPIC_API_KEY');
  const [code] = await exited;
  assert.equal(code, 1); assert.deepEqual(JSON.parse(out), {error: 'runtime_failed', partialAnswer: '', authKind: 'unknown', phase: 'input'}); assert.equal(err, 'ACP answer failed\n');
});
test('cleanup kills offline fake adapter and descendant without a separate group', async () => {
  const script = "const {spawn}=require('node:child_process'); const c=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'}); console.log(c.pid); setInterval(()=>{},1000)";
  const child = spawn(process.execPath, ['-e', script], {stdio: ['ignore', 'pipe', 'ignore'], detached: false});
  const exited = once(child, 'exit');
  await once(child.stdout, 'data');
  await cleanupAdapter(child);
  const [, signal] = await exited;
  assert.equal(signal, 'SIGKILL');
});

test('private failure record is bounded even after escaping and never copies provider errors', () => {
  assert.deepEqual(JSON.parse(failureOutput(new Error('secret'))), {error: 'runtime_failed', partialAnswer: '', authKind: 'unknown', phase: 'unknown'});
  assert.deepEqual(JSON.parse(failureOutput({safeCategory: 'model_unavailable', authKind: 'account', phase: 'model_selection'})), {error: 'model_unavailable', partialAnswer: '', authKind: 'account', phase: 'model_selection'});
  assert.deepEqual(JSON.parse(failureOutput({authKind: 'secret', phase: 'secret'})), {error: 'runtime_failed', partialAnswer: '', authKind: 'unknown', phase: 'unknown'});
  const output = failureOutput({safeCategory: 'secret', partialAnswer: '\0'.repeat(LIMITS.answer)});
  assert.ok(Buffer.byteLength(output) <= 64 * 1024);
  assert.equal(JSON.parse(output).error, 'runtime_failed');
});

test('raw subscription plan normalizes only known optional Claude prefix', () => {
  for (const plan of ['pro', 'max', 'team', 'enterprise', 'Claude Max', 'Claude Team', ' CLAUDE Pro ', 'Claude Enterprise']) {
    const state = collector(); state.auth({authStatus: {kind: 'account', account: {plan}}});
    assert.doesNotThrow(() => state.begin('s'));
  }
  for (const plan of ['Claude API', 'free', 'Claude Team extra', 'claudeteam', '', 'Claude Claude Max']) {
    const state = collector(); state.auth({authStatus: {kind: 'account', account: {plan}}});
    assert.throws(() => state.begin('s'));
  }
});
test('auth waits for notification, times out boundedly, and rejects known non-account', async () => {
  const state = collector(); let settled = false;
  const waiting = state.waitForAuth(1000).then(() => { settled = true; });
  await Promise.resolve(); assert.equal(settled, false);
  auth(state); await waiting; assert.equal(settled, true);
  const missing = collector(); await assert.rejects(missing.waitForAuth(1), {safeCategory: 'auth_required'});
  const denied = collector(); denied.auth({authStatus: {kind: 'api_key'}});
  await assert.rejects(denied.waitForAuth(1000), {safeCategory: 'auth_required'});
  const cancelled = collector(); const pending = cancelled.waitForAuth(1000); cancelled.cancelWaiters();
  await assert.rejects(pending, {safeCategory: 'auth_required'});
});
test('runSession does not prompt until delayed subscription notification arrives', async () => {
  const state = collector(), base = mock(state); const agent = {calls: [], async request(method, params) {
    this.calls.push(method);
    if (method === 'session/new') return {sessionId: 's', configOptions: [{id: 'model', options: [{value: 'sonnet'}]}]};
    if (method === 'session/set_config_option') setImmediate(() => { assert.ok(!this.calls.includes('session/prompt')); auth(state); });
    return base.request(method, params);
  }};
  assert.deepEqual(await runSession(agent, input, '/scratch', state), {answer, estimatedUsd: null});
});

test('first collector invalidation category survives later invalid events', () => {
  const reasons = []; const state = collector(reason => reasons.push(reason)); auth(state); state.begin('s');
  state.update({sessionId: 's', update: {sessionUpdate: 'current_model_update', currentModelId: 'opus'}});
  state.permission(); state.auth({authStatus: {kind: 'none'}});
  assert.deepEqual(reasons, ['model_changed']); assert.equal(state.invalidReason(), 'model_changed');
});
test('SDK diagnostics map only exact known templates and enum values', () => {
  for (const kind of ['error_max_turns', 'error_max_budget_usd', 'error_max_structured_output_retries', 'error_during_execution']) {
    assert.equal(sdkErrorCategory({data: {errorKind: kind}}), kind);
    assert.equal(sdkErrorCategory({message: kind}), kind);
    assert.equal(JSON.parse(failureOutput({safeCategory: kind})).error, kind);
  }
  assert.equal(sdkErrorCategory({message: 'Internal error: Reached maximum number of turns (2)'}), 'error_max_turns');
  assert.equal(sdkErrorCategory({message: 'Reached maximum budget ($1.00)'}), 'error_max_budget_usd');
  assert.equal(sdkErrorCategory({data: {errorKind: 'secret'}, message: 'error_max_turns secret'}), null);
  for (const category of ['model_changed', 'tool_attempt', 'nontext', 'wrong_session', 'oversize', 'auth_changed', 'timeout'])
    assert.equal(JSON.parse(failureOutput({safeCategory: category})).error, category);
});

test('known inference 401 is authentication failure even with account status', () => {
  assert.equal(sdkErrorCategory({message: 'API Error: 401 {"error":{"type":"authentication_error"}}'}), 'authentication_failed');
  assert.equal(sdkErrorCategory({message: 'Internal error: API Error: 401 unauthorized'}), 'authentication_failed');
  assert.equal(sdkErrorCategory({message: 'API Error: 4010 other'}), null);
  assert.equal(sdkErrorCategory({message: 'source mentions API Error: 401'}), null);
  const state = collector(); auth(state); state.begin('s'); chunk(state, 'API Error: 401 authentication rejected');
  assert.throws(() => state.finish({stopReason: 'end_turn'}, 'p'), {safeCategory: 'authentication_failed'});
  assert.equal(JSON.parse(failureOutput({safeCategory: 'authentication_failed'})).error, 'authentication_failed');
});

test('revoked OAuth inference credential has a fixed safe category and no retry', () => {
  const message = 'API Error: 401 {"error":{"type":"authentication_error","message":"OAuth access token has been revoked"}}';
  assert.equal(sdkErrorCategory({message}), 'auth_required');
  assert.equal(sdkErrorCategory({message: 'OAuth access token has been revoked'}), null);
  const state = collector(); auth(state); state.begin('s'); chunk(state, message);
  assert.throws(() => state.finish({stopReason: 'end_turn'}, 'p'), {safeCategory: 'auth_required'});
  const diagnostic = JSON.parse(failureOutput({safeCategory: 'auth_required', partialAnswer: message}));
  assert.equal(diagnostic.error, 'auth_required'); assert.equal(diagnostic.partialAnswer, '');
});

test('observed authentication prefix is classified without exposing provider text', () => {
  const message = 'Failed to authenticate. API Error: 401 OAuth access token has been revoked.';
  assert.equal(sdkErrorCategory({message}), 'auth_required');
  assert.equal(sdkErrorCategory({message: `source mentions ${message}`}), null);
  assert.equal(sdkErrorCategory({message: message.replace('401', '4010')}), null);
  const state = collector(); auth(state); state.begin('s'); chunk(state, message);
  assert.throws(() => state.finish({stopReason: 'end_turn'}, 'p'), {safeCategory: 'auth_required'});
  const output = JSON.parse(failureOutput({safeCategory: sdkErrorCategory({message}), partialAnswer: message}));
  assert.equal(output.error, 'auth_required');
  assert.equal(output.partialAnswer, '');
});
