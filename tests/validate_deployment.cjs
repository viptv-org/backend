#!/usr/bin/env node
// Static deployment validation. Never loads .env or talks to a Docker daemon.
// npm install --prefix artifacts/deployment-tools --ignore-scripts --no-audit --no-fund ajv@8.17.1 yaml@2.8.2
// NODE_PATH=artifacts/deployment-tools/node_modules node tests/validate_deployment.cjs path/to/compose-spec.json
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const YAML = require('yaml');
const Ajv2020 = require('ajv/dist/2020').default;
const root = path.resolve(__dirname, '..');
const schemaPath = process.argv[2];
assert(schemaPath, 'Supply a downloaded official Compose JSON schema path');
const read = name => fs.readFileSync(path.join(root, name), 'utf8');
function yaml(name) {
  const document = YAML.parseDocument(read(name), { version: '1.2', uniqueKeys: true });
  assert.equal(document.errors.length, 0, `${name}: ${document.errors.map(e => e.message).join('; ')}`);
  return document.toJS();
}
const schemaBytes = fs.readFileSync(schemaPath);
const schema = JSON.parse(schemaBytes);
const ajv = new Ajv2020({ strict: false, allErrors: true, validateFormats: false });
const validate = ajv.compile(schema);
const compose = yaml('compose.yaml');
assert(validate(compose), JSON.stringify(validate.errors, null, 2));
console.log('PASS Compose YAML conforms to the supplied official schema');
const service = compose.services.viptv;
assert.equal(service.user, '10001:10001');
assert.equal(service.read_only, true);
assert.equal(service.init, true);
assert(service.cap_drop.includes('ALL'));
assert(service.security_opt.includes('no-new-privileges:true'));
assert.equal(service.privileged, undefined);
assert.equal(service.network_mode, undefined);
const removedSharedKey = ['VIPTV', 'API', 'KEY'].join('_');
assert.equal(service.environment[removedSharedKey], undefined);
assert(!JSON.stringify(compose).includes(removedSharedKey));
assert(!JSON.stringify(compose).includes('/var/run/docker.sock'));
assert(service.volumes.includes('viptv_data:/data'));
assert(service.tmpfs.some(value => value.startsWith('/cache:') && value.includes('size=512m') && value.includes('uid=10001')));
assert.equal(service.cpus, undefined);
assert.equal(service.mem_limit, undefined);
assert.equal(service.memswap_limit, undefined);
assert.equal(service.pids_limit, undefined);
console.log('PASS nonroot, read-only, unthrottled CPU/memory/PID and secret-configuration invariants');
const main = read('server/src/main.rs') + '\n' + read('server/src/auth.rs');
for (const key of Object.keys(service.environment)) {
  assert(main.includes(`"${key}"`), `Compose environment ${key} has no matching backend setting`);
}
const dockerfile = read('Dockerfile');
const runtime = dockerfile.slice(dockerfile.lastIndexOf('FROM '));
assert(/^USER 10001:10001$/m.test(runtime));
assert(/^ENTRYPOINT \["\/usr\/local\/bin\/viptv-server"\]$/m.test(runtime));
assert(runtime.includes('/api/health'));
assert(runtime.includes('ca-certificates ffmpeg curl'));
assert(!new RegExp(`^(ARG|ENV)\\s+${removedSharedKey}`, 'm').test(dockerfile));
assert(dockerfile.includes('cargo build --release --locked'));
assert(dockerfile.includes('npm ci --no-audit --no-fund'));
const accountOnlyDeployment = [
  '.env.example', 'compose.yaml', 'compose.validation.yaml',
  'scripts/host-check.sh', 'scripts/container-check.sh', 'Dockerfile',
  '.github/workflows/ci.yml'
].map(read).join('\n');
assert(!accountOnlyDeployment.includes(removedSharedKey));
assert(!accountOnlyDeployment.includes('/auth/' + 'claim'));
console.log('PASS deployment files contain no shared-key or bootstrap-claim contract');
for (const name of ['server/Cargo.lock', 'dashboard/package-lock.json']) assert(fs.statSync(path.join(root, name)).isFile());
const ignored = read('.dockerignore').split(/\r?\n/);
for (const pattern of ['**', '**/.env', '**/.env.*', 'server/target/', 'dashboard/node_modules/']) assert(ignored.includes(pattern));
console.log('PASS Dockerfile, backend environment, lockfile and build-context safeguards');
const ci = yaml('.github/workflows/ci.yml');
assert.equal(ci.permissions.contents, 'read');
for (const job of ['server', 'dashboard', 'roku', 'image']) assert(ci.jobs[job]);
assert(ci.jobs.server.steps.some(step => (step.run || '').includes('--include-ignored')));
assert(ci.jobs.image.steps.some(step => (step.run || '').includes('config --quiet')));
assert(ci.jobs.image.steps.some(step => (step.run || '').includes('docker build')));
assert(ci.jobs.image.steps.some(step => (step.run || '').includes('/api/health')));
assert(!/docker\s+push/.test(JSON.stringify(ci)));
console.log('PASS CI declares component, real-media and nonpublishing image-health gates');
console.log('Schema SHA256:', crypto.createHash('sha256').update(schemaBytes).digest('hex'));
console.log('Static validation only: interpolation, image build, container startup and device behavior are NOT established.');
