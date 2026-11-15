#!/usr/bin/env python3
"""Copy the checked-in Pi chat catalog; never contacts services or reads credentials."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'legacy/pi/packages/ai/src/providers'
FIELDS = set('provider id name api baseUrl reasoning input cost contextWindow maxTokens thinkingLevelMap samplingParams compat'.split())
SPECIAL = {'anthropic', 'openai-codex', 'openai', 'google', 'google-vertex', 'mistral', 'azure', 'openrouter', 'github-copilot', 'amazon-bedrock', 'xai', 'radius', 'typesafe'}
OWNERS = {'openai': 'openai', 'openai-codex': 'openai', 'anthropic':'anthropic', 'google':'google', 'google-vertex':'google', 'mistral':'mistral', 'azure':'azure-openai', 'openrouter':'openrouter', 'github-copilot':'copilot', 'amazon-bedrock':'bedrock', 'xai':'xai'}

def models(provider, include_headers=False):
    data = json.loads((SOURCE / 'data' / (provider + '.json')).read_text())
    result = []
    for api, entries in data.items():
        for value in entries.values():
            if value.get('type', 'chat') != 'chat':
                continue
            fields = FIELDS | {'headers'} if include_headers else FIELDS
            result.append({key: value[key] for key in value if key in fields})
    return result

def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, separators=(',', ':')) + '\n')

def main():
    definitions = []
    for source in sorted(SOURCE.glob('*.ts')):
        provider = source.stem
        if provider in SPECIAL or not (SOURCE / 'data' / (provider + '.json')).exists():
            continue
        text = source.read_text()
        identity = re.search(r'id: "([^"]+)"', text)
        if not identity or identity[1] != provider:
            continue
        name = re.search(r'name: "([^"]+)"', text)[1]
        env = re.search(r'envApiKeyAuth\([^,]+,\s*\[([^]]+)\]', text)
        keys = re.findall(r'"([A-Z_0-9]+)"', env[1]) if env else ['CLOUDFLARE_API_KEY']
        entries = models(provider, include_headers=True)
        if not entries:
            continue
        definition = {'id': provider, 'name': name, 'environment': keys, 'models': entries}
        headers = {model['id']: model.pop('headers') for model in entries if 'headers' in model}
        if headers:
            definition['modelHeaders'] = headers
        definitions.append(definition)
    write(ROOT / 'plugins/providers/pi-plugin-gateways/src/data/catalog.json', definitions)
    for provider, owner in OWNERS.items():
        write(ROOT / f'plugins/providers/pi-plugin-{owner}/src/data/{provider}.json', models(provider))
    print(f'Synchronized {len(definitions)} gateway providers and {len(OWNERS)} dedicated catalogs')

if __name__ == '__main__':
    main()
