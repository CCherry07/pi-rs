import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import ts from 'typescript'

const SOURCE_ROOT = path.resolve(process.cwd(), 'src')
const CHECKED_EXTENSIONS = new Set(['.ts', '.tsx'])
const SKIPPED_FILE_SUFFIXES = ['.test.ts', '.test.tsx', '.d.ts']
const SKIPPED_DIRECTORIES = new Set(['i18n', 'test'])
const IGNORED_PARENT_TAGS = new Set(['code', 'kbd', 'pre'])
const USER_FACING_ATTRIBUTES = new Set([
  'aria-label',
  'ariaLabel',
  'alt',
  'cancelLabel',
  'confirmLabel',
  'description',
  'emptyText',
  'errorMessage',
  'helpText',
  'label',
  'message',
  'okLabel',
  'placeholder',
  'subtitle',
  'successMessage',
  'title',
  'data-tooltip',
  'tooltip',
])
const USER_FACING_PROPERTY_PATTERN = /(?:Label|Title|Subtitle|Description|Message|Placeholder|Text|Tooltip)$/
const USER_FACING_VARIABLE_PATTERN = /(?:aria|body|caption|description|detail|emptyText|helpText|hint|label|message|notice|placeholder|reason|subtitle|summary|title|tooltip)$/i
const USER_FACING_CALLS = new Set(['alert', 'ask', 'message', 'pushErrorToast', 'pushThreadErrorMessage', 'sendNotification'])
const EXEMPT_LITERALS = new Set([
  '&nbsp;',
  'AGENTS.md',
  'Alt',
  'Antigravity',
  'PI_AGENT_DIR/prompts',
  'Command',
  'Cmd',
  'Control',
  'Ctrl',
  'Create a practical custom coding agent configuration.',
  'Cursor',
  'Finder',
  'Git',
  'GitHub',
  'Ghostty',
  'Hook:',
  'JSON',
  'Markdown',
  'Meta',
  'Node:',
  'Option',
  'PATH:',
  'Pi',
  'README',
  'Shift',
  'Shift+Cmd+Enter',
  'Shift+Ctrl+Enter',
  'TCP ·',
  'Token',
  'URL',
  'VS Code',
  'Windows',
  'Zed',
  'compact',
  'completed',
  'fast',
  'failed',
  'fork',
  'mcp',
  'new',
  'notice',
  'px',
  'resume',
  'status',
  'steer',
  'apps',
  'create',
  'Collab:',
  'Collab tool call',
  'edit',
  'error',
  'event',
  'init error',
  'macbook.your-tailnet.ts.net:4732',
  'main',
  'medium',
  'option',
  'alt',
  'pnpm install',
  'researcher',
  'short',
  'warning',
  '{diff}',
  'Agent name:',
  'Description seed:',
  'Developer instructions seed:',
])

function listSourceFiles(directory) {
  const entries = fs.readdirSync(directory, { withFileTypes: true })
  return entries.flatMap(entry => {
    const fullPath = path.join(directory, entry.name)
    if (entry.isDirectory()) {
      return SKIPPED_DIRECTORIES.has(entry.name) ? [] : listSourceFiles(fullPath)
    }
    if (!CHECKED_EXTENSIONS.has(path.extname(entry.name))) {
      return []
    }
    if (SKIPPED_FILE_SUFFIXES.some(suffix => entry.name.endsWith(suffix))) {
      return []
    }
    return [fullPath]
  })
}

function normalizeText(value) {
  return value.replace(/\s+/g, ' ').trim()
}

function containsNaturalLanguage(value) {
  const normalized = normalizeText(value)
  return /[A-Za-z]{2}/.test(normalized) && !EXEMPT_LITERALS.has(normalized) && !normalized.startsWith('http://') && !normalized.startsWith('https://')
}

function getJsxTagName(node) {
  const parent = node.parent
  if (!parent || !ts.isJsxElement(parent)) {
    return null
  }
  return parent.openingElement.tagName.getText()
}

function isTranslationCall(node) {
  const expression = ts.isCallExpression(node) ? node.expression.getText() : ''
  return ts.isCallExpression(node) && (expression === 't' || /^t[A-Z]\w*$/.test(expression) || expression.endsWith('.t'))
}

function isMachineLiteralNode(node) {
  const parent = node.parent
  if (!parent) {
    return false
  }
  if (ts.isBinaryExpression(parent)) {
    const comparisonOperators = new Set([
      ts.SyntaxKind.EqualsEqualsToken,
      ts.SyntaxKind.EqualsEqualsEqualsToken,
      ts.SyntaxKind.ExclamationEqualsToken,
      ts.SyntaxKind.ExclamationEqualsEqualsToken,
      ts.SyntaxKind.InKeyword,
    ])
    if (comparisonOperators.has(parent.operatorToken.kind)) {
      return true
    }
  }
  if (ts.isCaseClause(parent)) {
    return true
  }
  if (ts.isPropertyAssignment(parent)) {
    const propertyName = propertyNameText(parent.name)
    if (['id', 'kind', 'role', 'source', 'status', 'type'].includes(propertyName)) {
      return true
    }
  }
  return false
}

function isTechnicalContextLiteral(value) {
  const normalized = normalizeText(value)
  return (
    normalized.includes('/') ||
    normalized.includes('\\') ||
    /^[a-z][a-z0-9+.-]*:$/.test(normalized) ||
    /^[a-z0-9][a-z0-9.-]*:\d+$/.test(normalized) ||
    /^-[-_a-zA-Z0-9]+$/.test(normalized) ||
    /^[-_a-zA-Z0-9]+-$/.test(normalized) ||
    /^(?:[a-z][a-zA-Z0-9_-]*\.)+[a-zA-Z0-9_-]+$/.test(normalized) ||
    /^--\S+/.test(normalized) ||
    /^[a-z0-9]+(?:-[a-z0-9]+)+$/.test(normalized)
  )
}

function propertyNameText(name) {
  if (!name) {
    return ''
  }
  if (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNumericLiteral(name)) {
    return name.text
  }
  return name.getText()
}

function literalText(node) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
    return node.text
  }
  if (node.kind === ts.SyntaxKind.TemplateHead || node.kind === ts.SyntaxKind.TemplateMiddle || node.kind === ts.SyntaxKind.TemplateTail) {
    return node.text
  }
  return null
}

function collectUntranslatedLiterals(root) {
  const literals = []

  function visit(node) {
    if (node !== root && (ts.isJsxElement(node) || ts.isJsxSelfClosingElement(node) || ts.isJsxFragment(node) || ts.isJsxAttribute(node))) {
      return
    }
    if (isTranslationCall(node)) {
      return
    }
    const value = literalText(node)
    if (value !== null) {
      if (containsNaturalLanguage(value) && !isMachineLiteralNode(node) && !isTechnicalContextLiteral(value)) {
        literals.push({ node, value })
      }
      return
    }
    ts.forEachChild(node, visit)
  }

  visit(root)
  return literals
}

function reportForNode(sourceFile, node, kind, value) {
  const position = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile))
  return {
    file: path.relative(process.cwd(), sourceFile.fileName),
    line: position.line + 1,
    column: position.character + 1,
    kind,
    value: normalizeText(value),
  }
}

function auditFile(filePath) {
  const source = fs.readFileSync(filePath, 'utf8')
  const sourceFile = ts.createSourceFile(filePath, source, ts.ScriptTarget.Latest, true, filePath.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const findings = []

  function visit(node) {
    if (ts.isJsxText(node) && containsNaturalLanguage(node.text)) {
      const parentTag = getJsxTagName(node)
      if (!parentTag || !IGNORED_PARENT_TAGS.has(parentTag)) {
        findings.push(reportForNode(sourceFile, node, 'jsx-text', node.text))
      }
    }

    if (ts.isJsxAttribute(node)) {
      const attributeName = node.name.getText()
      if (USER_FACING_ATTRIBUTES.has(attributeName) && node.initializer) {
        if (ts.isStringLiteral(node.initializer) && containsNaturalLanguage(node.initializer.text) && !isTechnicalContextLiteral(node.initializer.text)) {
          findings.push(reportForNode(sourceFile, node.initializer, `jsx-attribute:${attributeName}`, node.initializer.text))
        } else if (ts.isJsxExpression(node.initializer) && node.initializer.expression && !isTranslationCall(node.initializer.expression)) {
          for (const { node: literal, value } of collectUntranslatedLiterals(node.initializer.expression)) {
            findings.push(reportForNode(sourceFile, literal, `jsx-attribute:${attributeName}`, value))
          }
        }
      }
    }

    if (ts.isJsxExpression(node) && !ts.isJsxAttribute(node.parent) && node.expression && !isTranslationCall(node.expression)) {
      const parentTag = getJsxTagName(node)
      if (!parentTag || !IGNORED_PARENT_TAGS.has(parentTag)) {
        for (const { node: literal, value } of collectUntranslatedLiterals(node.expression)) {
          findings.push(reportForNode(sourceFile, literal, 'jsx-expression', value))
        }
      }
    }

    if (ts.isPropertyAssignment(node)) {
      const name = propertyNameText(node.name)
      if (!name.startsWith('on') && (USER_FACING_ATTRIBUTES.has(name) || USER_FACING_PROPERTY_PATTERN.test(name)) && !isTranslationCall(node.initializer)) {
        for (const { node: literal, value } of collectUntranslatedLiterals(node.initializer)) {
          findings.push(reportForNode(sourceFile, literal, `property:${name}`, value))
        }
      }
    }

    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      /^[a-z]/.test(node.name.text) &&
      node.initializer &&
      !ts.isArrowFunction(node.initializer) &&
      !ts.isFunctionExpression(node.initializer) &&
      !/^(?:build|format|generate|handle|has|is|push|send|set|to)/.test(node.name.text) &&
      USER_FACING_VARIABLE_PATTERN.test(node.name.text)
    ) {
      for (const { node: literal, value } of collectUntranslatedLiterals(node.initializer)) {
        findings.push(reportForNode(sourceFile, literal, `variable:${node.name.text}`, value))
      }
    }

    if (ts.isCallExpression(node)) {
      const callName = node.expression.getText().split('.').at(-1)
      const isUserFacingSetter = Boolean(callName && /^set\w*(?:Error|Message|StatusText)$/.test(callName))
      if (callName && (USER_FACING_CALLS.has(callName) || isUserFacingSetter)) {
        for (const argument of node.arguments) {
          for (const { node: literal, value } of collectUntranslatedLiterals(argument)) {
            findings.push(reportForNode(sourceFile, literal, `call:${callName}`, value))
          }
        }
      }
    }

    ts.forEachChild(node, visit)
  }

  visit(sourceFile)
  return findings
}

const findings = listSourceFiles(SOURCE_ROOT)
  .flatMap(auditFile)
  .filter(
    (finding, index, all) =>
      all.findIndex(candidate => candidate.file === finding.file && candidate.line === finding.line && candidate.column === finding.column && candidate.value === finding.value) === index,
  )

for (const finding of findings) {
  process.stdout.write(`${finding.file}:${finding.line}:${finding.column} [${finding.kind}] ${finding.value}\n`)
}

if (findings.length > 0) {
  process.stderr.write(`\nFound ${findings.length} likely untranslated UI strings.\n`)
  process.exitCode = 1
} else {
  process.stdout.write('No likely untranslated UI strings found.\n')
}
