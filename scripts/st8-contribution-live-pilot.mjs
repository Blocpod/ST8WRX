#!/usr/bin/env node
// Isolated loopback pilot. Uses the desktop's installed nostr-tools; never stores keys.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../desktop/package.json', import.meta.url));
const { finalizeEvent, generateSecretKey, getPublicKey, verifyEvent } = require('nostr-tools');
const [mode, base, statePath] = process.argv.slice(2);
assert(['seed', 'verify'].includes(mode) && base && statePath,
  'usage: node scripts/st8-contribution-live-pilot.mjs <seed|verify> <http://127.0.0.1:PORT> <public-state.json>');
const url = new URL(base);
assert(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port && url.port !== '3000'
  && url.pathname === '/' && !url.search && !url.hash && !url.username && !url.password,
  'use a dedicated loopback relay on a non-default port');
assert.equal(process.env.ST8_PILOT_CONFIRM_ISOLATED, '1', 'set ST8_PILOT_CONFIRM_ISOLATED=1 only for your isolated pilot database/relay');
const now = () => Math.floor(Date.now() / 1000);
const hash = (bytes) => createHash('sha256').update(bytes).digest();
const sign = (key, kind, tags, content = '', created_at = now()) => finalizeEvent({ kind, tags, content, created_at }, key);
async function post(path, value, key) {
  const body = JSON.stringify(value);
  const endpoint = new URL(path, url).href;
  const auth = sign(key, 27235, [['u', endpoint], ['method', 'POST'], ['payload', hash(body).toString('hex')], ['nonce', randomUUID()]]);
  const response = await fetch(endpoint, { method: 'POST', headers: {
    'content-type': 'application/json', authorization: `Nostr ${Buffer.from(JSON.stringify(auth)).toString('base64')}`,
  }, body, signal: AbortSignal.timeout(15000) });
  const result = await response.json();
  assert(response.ok, `${path} ${response.status}: ${JSON.stringify(result)}`);
  return result;
}
async function publish(event, key) {
  assert(verifyEvent(event), 'fixture event signature');
  const ack = await post('/events', event, key);
  assert.equal(ack.accepted, true, JSON.stringify(ack));
}
// Independent encoding of this one fixture, checked by the relay's Rust protocol engine.
const integer = (n, width) => { const bytes = Buffer.alloc(width); bytes.writeUIntBE(n, 0, width); return bytes; };
const int64 = (n) => { const bytes = Buffer.alloc(8); bytes.writeBigInt64BE(BigInt(n)); return bytes; };
const field = (value) => { const bytes = Buffer.from(value); return Buffer.concat([integer(bytes.length, 4), bytes]); };
async function seed() {
  const owner = generateSecretKey();
  const founder = generateSecretKey();
  const pubkey = getPublicKey(owner);
  const created = now();
  const slug = `pilot-${randomUUID()}`;
  const project = `30621:${pubkey}:${slug}`;
  const projectEvent = sign(owner, 30621, [['d', slug], ['name', 'Isolated beta pilot'], ['buzz-visibility', 'listed']], '', created);
  const policy = { project, version: 'pilot-v1', founders: [`nostr:${pubkey}`, `nostr:${getPublicKey(founder)}`], normal_threshold: 2, large_threshold: 2, large_unit_threshold: 100 };
  const policyEvent = sign(owner, 30624, [['a', project], ['d', policy.version]], JSON.stringify(policy), created);
  const summary = 'Isolated signed contribution pilot';
  const claim = sign(owner, 49801, [['a', project], ['e', projectEvent.id]], JSON.stringify({ contributor_kind: 'Human', class: 'Engineering', summary }), created);
  const evidence = Buffer.concat([integer(1, 2), Buffer.from(projectEvent.id, 'hex'), field(`nostr:${projectEvent.id}`)]);
  const contributionId = hash(Buffer.concat([Buffer.from('ST8WRX\0CONTRIBUTION_RECORD\0V1'), field(project), field(`nostr:${pubkey}`), integer(1, 2), integer(3, 2), int64(created), field(summary), integer(1, 4), field(evidence)]));
  const intent = { contribution_id: [...contributionId], project, status: 'Accepted', contribution_units: 7, policy_version: policy.version, decided_at: created + 600, rationale: 'Reviewed isolated pilot' };
  const digest = hash(Buffer.concat([Buffer.from('ST8WRX\0GOVERNANCE_INTENT\0V1'), contributionId, field(project), integer(1, 2), int64(7), field(policy.version), int64(intent.decided_at), field(intent.rationale)])).toString('hex');
  const common = [['a', project], ['st8-contribution', contributionId.toString('hex')], ['st8-policy', policy.version], ['st8-decision', digest]];
  const proposal = sign(owner, 49802, [...common, ['st8-policy-event', policyEvent.id], ['e', claim.id]], JSON.stringify(intent), created);
  const first = sign(owner, 49803, [...common, ['e', proposal.id]], digest, created);
  const second = sign(founder, 49803, [...common, ['e', proposal.id]], digest, created);
  for (const event of [projectEvent, policyEvent, claim, proposal, first]) await publish(event, owner);
  const filter = { kinds: [30625], '#a': [project], limit: 10 };
  assert.equal((await post('/query', [filter], owner)).length, 0, 'one of two founders must not award units');
  await publish(second, founder);
  await publish(second, founder); // Same signed event must not issue a second award.
  const result = { base: url.origin, project, contributionId: contributionId.toString('hex'), expectedUnits: 7,
    claimId: claim.id, proposalId: proposal.id, approvalIds: [first.id, second.id] };
  await writeFile(statePath, `${JSON.stringify(result, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  return result;
}
async function websocketQuery(filter, key) {
  const endpoint = url.href.replace('http:', 'ws:');
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(endpoint);
    const timer = setTimeout(() => finish(new Error('WebSocket AUTH/REQ timeout')), 15000);
    const events = [];
    let authId;
    let done = false;
    function finish(error) {
      if (done) return;
      done = true;
      clearTimeout(timer);
      ws.close();
      if (error) reject(error); else resolve(events);
    }
    ws.onerror = () => finish(new Error('WebSocket connection error'));
    ws.onmessage = ({ data }) => {
      try {
        const frame = JSON.parse(data);
        if (frame[0] === 'AUTH') {
          const event = sign(key, 22242, [['relay', endpoint.replace(/\/$/, '')], ['challenge', frame[1]]]);
          authId = event.id;
          ws.send(JSON.stringify(['AUTH', event]));
        } else if (frame[0] === 'OK' && frame[1] === authId) {
          assert.equal(frame[2], true, JSON.stringify(frame));
          ws.send(JSON.stringify(['REQ', 'pilot-ledger', filter]));
        } else if (frame[0] === 'EVENT') events.push(frame[2]);
        else if (frame[0] === 'EOSE') finish();
        else if (frame[0] === 'CLOSED') finish(new Error(JSON.stringify(frame)));
      } catch (error) { finish(error); }
    };
  });
}
const state = mode === 'seed' ? await seed() : JSON.parse(await readFile(statePath, 'utf8'));
assert.equal(state.base, url.origin, 'state belongs to a different pilot relay');
const queryKey = generateSecretKey();
const filter = { kinds: [30625], '#a': [state.project], limit: 10 };
const httpEvents = await post('/query', [filter], queryKey);
assert.equal(httpEvents.length, 1, 'exactly one durable ledger entry');
const ledger = JSON.parse(httpEvents[0].content);
assert.equal(ledger.contribution_units, state.expectedUnits);
assert.equal(ledger.claim_event_id, state.claimId);
assert.deepEqual([...ledger.approval_event_ids].sort(), [...state.approvalIds].sort());
const wsEvents = await websocketQuery(filter, queryKey);
assert.equal(wsEvents.length, 1, 'WebSocket query returns one ledger entry');
assert(verifyEvent(wsEvents[0]), 'relay ledger signature verifies');
assert.equal(wsEvents[0].id, httpEvents[0].id, 'HTTP and WebSocket ledger IDs agree');
if (state.ledgerId) assert.equal(wsEvents[0].id, state.ledgerId, 'restart must preserve exact ledger event');
else await writeFile(statePath, `${JSON.stringify({ ...state, ledgerId: wsEvents[0].id }, null, 2)}\n`, { mode: 0o600 });
console.log(JSON.stringify({ mode, project: state.project, ledgerId: wsEvents[0].id, units: ledger.contribution_units, http: 'passed', websocket: 'passed', noWalletCalls: true }));
