export interface ConfigModel {
  name: string
  display_name: string
  max_tokens: number
  temperature: number
  context_window: number
  enabled: boolean
  provider_capabilities: Record<string, unknown>
}

export interface ConfigPlatform {
  name: string
  base_url: string
  api_key?: string | null
  models: ConfigModel[]
}

export interface ModelConfig {
  version: number
  platforms: ConfigPlatform[]
}

export interface SelectablePlatform {
  selected: boolean
  platform: ConfigPlatform
  models: { selected: boolean; model: ConfigModel }[]
}

export function selectConfig(config: ModelConfig): SelectablePlatform[] {
  return config.platforms.map(platform => ({
    selected: true,
    platform,
    models: platform.models.map(model => ({ selected: true, model })),
  }))
}

export function selectedConfig(rows: SelectablePlatform[], includeKeys: boolean): ModelConfig {
  return {
    version: 1,
    platforms: rows.filter(row => row.selected).map(({ platform, models }) => ({
      name: platform.name,
      base_url: platform.base_url,
      ...(includeKeys && platform.api_key != null ? { api_key: platform.api_key } : {}),
      models: models.filter(item => item.selected).map(item => ({ ...item.model })),
    })),
  }
}

export function toggleConfigPlatform(row: SelectablePlatform, selected: boolean) {
  row.selected = selected
  row.models.forEach(item => { item.selected = selected })
}

export function toggleConfigModel(
  row: SelectablePlatform,
  item: SelectablePlatform['models'][number],
  selected: boolean,
) {
  item.selected = selected
  if (selected) {
    row.selected = true
  } else if (row.models.length > 0 && !row.models.some(m => m.selected)) {
    row.selected = false
  }
}

export interface FilteredConfigPlatform {
  row: SelectablePlatform
  sourceIndex: number
  models: SelectablePlatform['models']
}

export function filterConfigPlatforms(rows: SelectablePlatform[], query: string): FilteredConfigPlatform[] {
  const keyword = query.trim().toLocaleLowerCase()
  return rows.flatMap((row, sourceIndex) => {
    const platformMatches = !keyword || [row.platform.name, row.platform.base_url]
      .some(value => value.toLocaleLowerCase().includes(keyword))
    const models = platformMatches ? row.models : row.models.filter(({ model }) =>
      [model.name, model.display_name].some(value => (value || '').toLocaleLowerCase().includes(keyword)),
    )
    return platformMatches || models.length ? [{ row, sourceIndex, models }] : []
  })
}

export function toggleConfigMatch(group: FilteredConfigPlatform, selected: boolean) {
  if (group.models.length === group.row.models.length) {
    toggleConfigPlatform(group.row, selected)
    return
  }
  group.models.forEach(item => { item.selected = selected })
  if (selected) group.row.selected = true
  else if (!group.row.models.some(item => item.selected)) group.row.selected = false
}

export const MODEL_CONFIG_EXAMPLE = JSON.stringify({
  version: 1,
  platforms: [{
    name: '我的平台',
    base_url: 'https://api.example.com/v1',
    api_key: '替换为你的 API Key（无需 Key 的本地服务可留空）',
    models: [{ name: '替换为平台的模型标识', display_name: '我的模型', context_window: 128000, max_tokens: 8192 }],
  }],
}, null, 2)
