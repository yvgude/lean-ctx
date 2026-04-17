import fs from 'node:fs';
import path from 'node:path';

const LOCALES = ['en', 'de', 'ar', 'zh', 'hi', 'es', 'fr', 'bn', 'pt', 'ru', 'ja'];
const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const LOCALES_DIR = path.join(ROOT, 'src', 'i18n', 'locales');

const DYNAMIC_PLACEHOLDER_KEYS = {
  'compatibility.fullMcpLabel': ['mcpToolCount'],
  'compatibility.mcpServerFeatureDesc': ['mcpToolCount', 'mcpToolMoreCount'],
  'docs.gettingStarted.injectReadWhy': ['readModeCount'],
  'docs.gettingStarted.intro': ['mcpToolCount'],
  'docs.gettingStarted.nextStepToolsLink': ['mcpToolCount'],
  'docs.gettingStarted.step3Check3': ['mcpToolCount'],
  'docs.tools.coreReadDesc': ['readModeCount'],
  'docs.tools.description': ['mcpToolCount'],
  'docs.tools.intro': ['mcpToolCount'],
  'docsGettingStarted.editorAntigravityStep4': ['mcpToolCount'],
  'docsGettingStarted.editorClaudeNote': ['mcpToolCount'],
  'docsGettingStarted.editorCursorStep4': ['mcpToolCount'],
  'docsGettingStarted.editorGeminiStep4': ['mcpToolCount'],
  'docsGettingStarted.editorWindsurfStep4': ['mcpToolCount'],
  'docsGettingStarted.injectReadWhy': ['readModeCount'],
  'docsGettingStarted.intro': ['mcpToolCount'],
  'docsGettingStarted.nextStepToolsLink': ['mcpToolCount'],
  'docsGettingStarted.verifyStep3': ['mcpToolCount'],
  'howItWorks.layer1Desc': ['mcpToolCount', 'readModeCount'],
  'index.archServerDesc': ['mcpToolCount'],
  'manifest.archLayer2Desc': ['readModeCount'],
  'manifest.archP2': ['mcpToolCount'],
  'manifest.principle4Desc': ['mcpToolCount'],
  'mcpServer.categoriesLabel': ['mcpToolCount'],
  'mcpServer.ctaDesc': ['mcpToolCount'],
  'mcpServer.heroTitle': ['mcpToolCount'],
  'mcpServer.metaDescription': ['mcpToolCount'],
  'mcpServer.readModesTitle': ['readModeCount'],
  'mcpServer.title': ['mcpToolCount'],
  'nav.docs.toolApiDesc': ['mcpToolCount'],
  'nav.product.contextServerDesc': ['mcpToolCount'],
};

function readJson(filePath) {
  const raw = fs.readFileSync(filePath, 'utf-8');
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new Error(`Failed to parse JSON: ${filePath}\n${e?.message ?? e}`);
  }
}

function extractPlaceholders(text) {
  const re = /\{([a-zA-Z0-9_]+)\}/g;
  const out = new Set();
  let m;
  while ((m = re.exec(text)) !== null) out.add(m[1]);
  return out;
}

function assertHasPlaceholders(locale, key, value, required) {
  const present = extractPlaceholders(value);
  for (const ph of required) {
    if (!present.has(ph)) {
      throw new Error(`[${locale}] key "${key}" missing placeholder "{${ph}}": ${JSON.stringify(value)}`);
    }
  }
}

function main() {
  const errors = [];
  const enPath = path.join(LOCALES_DIR, 'en.json');
  const en = readJson(enPath);
  const requiredKeys = Object.keys(DYNAMIC_PLACEHOLDER_KEYS);

  for (const locale of LOCALES) {
    const filePath = path.join(LOCALES_DIR, `${locale}.json`);
    if (!fs.existsSync(filePath)) {
      errors.push(`[${locale}] missing locale file: ${filePath}`);
      continue;
    }
    const data = readJson(filePath);
    for (const key of requiredKeys) {
      if (typeof en[key] !== 'string') errors.push(`[en] missing key or non-string: ${key}`);
      if (typeof data[key] !== 'string') {
        errors.push(`[${locale}] missing key or non-string: ${key}`);
        continue;
      }
      const required = DYNAMIC_PLACEHOLDER_KEYS[key];
      try {
        assertHasPlaceholders(locale, key, data[key], required);
      } catch (e) {
        errors.push(String(e.message ?? e));
      }
    }
  }

  if (errors.length) {
    console.error(errors.join('\n'));
    process.exit(1);
  }

  console.log('i18n ok');
}

main();

