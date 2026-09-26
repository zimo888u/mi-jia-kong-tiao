// Refresh public MIoT fixtures. No Xiaomi account or device credentials required.
import { writeFile, mkdir } from 'node:fs/promises';
const base = 'https://miot-spec.org/miot-spec-v2/';
async function get(url) {
  const response = await fetch(url, { signal: AbortSignal.timeout(60000) });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}
const catalog = await get(base + 'instances?status=all');
const models = new Map();
const rank = status => ({released: 3, preview: 2, debug: 1})[status] ?? 0;
for (const entry of catalog.instances) {
  if (!entry.model || !entry.type?.includes(':device:air-conditioner:')) continue;
  if (!/^(xiaomi|zhimi|viomi)\.(airc|aircondition)\./.test(entry.model)) continue;
  const previous = models.get(entry.model);
  if (!previous || rank(entry.status) > rank(previous.status)
      || (rank(entry.status) === rank(previous.status)
          && Number(entry.type.split(':').at(-1)) > Number(previous.type.split(':').at(-1)))) {
    models.set(entry.model, entry);
  }
}
// Include older ma/mh models and modern h-series; use exact official model keys.
const all = [...models].sort(([a],[b]) => a.localeCompare(b));
const selected = [...new Map([
  ...all.filter(([model]) => /\.(ma[1-9]|mh[1-9]|v[1-9]|h[1235][0-9]h[0-9][0-9])$/.test(model)),
  ...all.filter(([model]) => model.startsWith('viomi.')).slice(0, 8),
  ...all.filter(([model]) => model.startsWith('zhimi.')).slice(0, 8),
]).entries()].slice(0, 60);
const bundle = {};
const failures = [];
for (let offset = 0; offset < selected.length; offset += 4) {
  await Promise.all(selected.slice(offset, offset+4).map(async ([model,entry]) => {
    try {
      const urn = entry.type;
      const spec = await get(base + 'instance?type=' + encodeURIComponent(urn));
      if (spec.type !== urn) throw new Error('spec type does not match catalog');
      const services = spec.services?.map(({iid,type,description,properties}) => ({iid,type,description,properties}));
      bundle[model] = { type: spec.type, status: entry.status, description: spec.description, services };
      console.log(model);
    } catch (error) { failures.push(`${model}: ${error.message}`); }
  }));
}
if (failures.length || Object.keys(bundle).length !== selected.length) {
  throw new Error(`Specification refresh incomplete; original bundle unchanged. ${failures.join('; ')}`);
}
await mkdir('crates/miac-core/specs', {recursive:true});
await writeFile('crates/miac-core/specs/air-conditioners.json', JSON.stringify(Object.fromEntries(Object.entries(bundle).sort())) + '\n');
console.log(`Downloaded ${Object.keys(bundle).length} model specifications.`);
