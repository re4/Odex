#!/usr/bin/env node
// Tiny stdio MCP server for the desktop e2e tests (newline-delimited JSON-RPC).
// Tools: echo (read-only), add. One resource (fixture://readme) and one prompt (greet).
import process from 'node:process'
import readline from 'node:readline'

const TOOLS = [
  {
    name: 'echo',
    description: 'Echo text back',
    inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    annotations: { readOnlyHint: true },
  },
  {
    name: 'add',
    description: 'Add two numbers',
    inputSchema: { type: 'object', properties: { a: { type: 'number' }, b: { type: 'number' } }, required: ['a', 'b'] },
  },
]
const README = '# Fixture server\n\nThis resource comes from the Odex e2e MCP fixture.\n'

function send(msg) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...msg }) + '\n')
}

function handle(method, params) {
  switch (method) {
    case 'initialize':
      return {
        protocolVersion: params?.protocolVersion ?? '2025-06-18',
        capabilities: { tools: {}, resources: {}, prompts: {} },
        serverInfo: { name: 'odex-e2e-fixture', version: '1.2.3' },
        instructions: 'Fixture server (FIXTURE-INSTRUCTIONS-42): use echo to repeat text and add to sum two numbers.',
      }
    case 'ping':
      return {}
    case 'tools/list':
      return { tools: TOOLS }
    case 'tools/call': {
      const args = params?.arguments ?? {}
      if (params?.name === 'echo') return { content: [{ type: 'text', text: String(args.text ?? '') }] }
      if (params?.name === 'add') return { content: [{ type: 'text', text: String(Number(args.a) + Number(args.b)) }] }
      return { content: [{ type: 'text', text: `unknown tool ${params?.name}` }], isError: true }
    }
    case 'resources/list':
      return { resources: [{ uri: 'fixture://readme', name: 'readme', description: 'Fixture readme', mimeType: 'text/markdown' }] }
    case 'resources/templates/list':
      return { resourceTemplates: [] }
    case 'resources/read':
      return { contents: [{ uri: params?.uri, mimeType: 'text/markdown', text: README }] }
    case 'prompts/list':
      return { prompts: [{ name: 'greet', description: 'Say hello to someone', arguments: [{ name: 'who', required: false }] }] }
    case 'prompts/get':
      return { messages: [{ role: 'user', content: { type: 'text', text: `Say hello to ${params?.arguments?.who ?? 'everyone'}` } }] }
    default:
      throw Object.assign(new Error(`method not found: ${method}`), { code: -32601 })
  }
}

process.stderr.write('fixture MCP server started\n')
const rl = readline.createInterface({ input: process.stdin })
rl.on('line', (line) => {
  if (!line.trim()) return
  let msg
  try {
    msg = JSON.parse(line)
  } catch {
    return
  }
  if (msg.id === undefined || msg.id === null || !msg.method) return // notifications and responses
  try {
    send({ id: msg.id, result: handle(msg.method, msg.params) })
  } catch (e) {
    send({ id: msg.id, error: { code: e.code ?? -32603, message: e.message } })
  }
})
rl.on('close', () => process.exit(0))
