// @amp-agent-mode {"key":"kimi","label":"kimi"}
// @amp-agent-mode {"key":"glm","label":"glm"}
// @amp-agent-mode {"key":"deepseek","label":"deepseek"}
// @amp-agent-mode {"key":"gemini","label":"gemini"}
// @amp-agent-mode {"key":"sol","label":"sol"}
import type { PluginAPI } from '@ampcode/plugin'

export const description = 'Firstmate worker modes on non-default models (Kimi K3, GLM-5.3, DeepSeek V4.1 Flash, Gemini 3.8 Flash, GPT-6.1 Sol), built on the high mode.'

const MODES = [
	{ key: 'kimi', model: 'moonshotai/kimi-k3', color: '#0ea5e9', what: 'Kimi K3' },
	{ key: 'glm', model: 'zhipuai/glm-5.3', color: '#16a34a', what: 'GLM-5.3' },
	{ key: 'deepseek', model: 'deepseek/deepseek-v4.1-flash', color: '#2563eb', what: 'DeepSeek V4.1 Flash' },
	{ key: 'gemini', model: 'google-vertex/gemini-3.8-flash', color: '#d97706', what: 'Gemini 3.8 Flash' },
	{ key: 'sol', model: 'openai/gpt-6.1-sol', color: '#7c3aed', what: 'GPT-6.1 Sol' },
] as const

export default function (amp: PluginAPI) {
	for (const m of MODES) {
		const agent = amp.createAgent({
			extends: 'high',
			model: m.model,
			reasoningEffort: 'high',
			display: { label: m.key, color: m.color },
		})
		amp.registerAgentMode({
			key: m.key,
			description: `The high mode's prompt and tools on ${m.what}. Use for autonomous coding tasks routed by firstmate.`,
			agent: agent.definition,
		})
	}
}
