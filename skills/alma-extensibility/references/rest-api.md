# Alma REST API Reference

**Base URL:** `http://localhost:23001`  
**Full spec:** `~/.config/alma/api-spec.md` (read for complete data types)

## Endpoints

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/api/health` | Returns `{status, timestamp}` |
| `GET` | `/api/settings` | Full AppSettings object |
| `PUT` | `/api/settings` | ⚠️ Requires COMPLETE object — always GET first |
| `POST` | `/api/settings/reset` | Reset all to defaults |
| `GET` | `/api/providers` | List AI providers |
| `POST` | `/api/providers` | Create provider (`{name, type, apiKey, baseURL?, enabled}`) |
| `PUT` | `/api/providers/:id` | Partial update supported |
| `DELETE` | `/api/providers/:id` | HTTP 204 |
| `POST` | `/api/providers/:id/test` | Test connectivity |
| `GET` | `/api/providers/:id/models` | Provider's models with capabilities |
| `POST` | `/api/providers/:id/models/fetch` | Re-fetch from provider API |
| `PUT` | `/api/providers/:id/models` | Update enabled models |
| `GET` | `/api/models` | All models (format: `providerId:modelId`) |
| `GET` | `/api/threads` | List all threads |
| `POST` | `/api/threads` | Create thread (`{"title": "name"}`) — appears in GUI sidebar |

## Provider Types

`openai`, `anthropic`, `google`, `aihubmix`, `openrouter`, `deepseek`, `copilot`, `azure`, `moonshot`, `volcengine`, `custom`, `acp`, `claude-subscription`, `zai-coding-plan`, `kimi-coding-plan`

## Key Rules

- **Settings PUT**: Must send the complete object. Partial updates fail.
- **Model IDs**: Always `providerId:modelId` format (e.g., `abc123:gpt-4o`).
- **API keys**: Encrypted in storage, never exposed in responses.
- **WebSocket sync**: API changes broadcast to connected GUI clients.
- **Errors**: Failed requests return `{"error": "message"}`.

## Usage Examples

### Settings (GET → modify → PUT)
```bash
current=$(curl -s http://localhost:23001/api/settings)
updated=$(echo "$current" | jq '.general.theme = "dark"')
curl -s -X PUT http://localhost:23001/api/settings \
  -H "Content-Type: application/json" -d "$updated"
```

### Thread Management
```bash
# Create — appears in GUI sidebar
curl -s -X POST http://localhost:23001/api/threads \
  -H "Content-Type: application/json" \
  -d '{"title": "QQ群 123456"}'

# List
curl -s http://localhost:23001/api/threads | jq '.[].title'
```

### Provider Management
```bash
# Create
curl -s -X POST http://localhost:23001/api/providers \
  -H "Content-Type: application/json" \
  -d '{"name": "My OpenAI", "type": "openai", "apiKey": "sk-xxx"}'

# Test connection
curl -s -X POST http://localhost:23001/api/providers/PROVIDER_ID/test
```

For full data types (`AppSettings`, `Provider`, `StoredProviderModel`), read `~/.config/alma/api-spec.md`.

## Related References

- [cli-reference.md](cli-reference.md) — CLI commands that wrap these REST endpoints
- [channel-bridge.md](channel-bridge.md) — How bridges use thread creation endpoints
- [people-profiles.md](people-profiles.md) — User profile files managed outside the API
