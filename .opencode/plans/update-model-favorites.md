# Update Model Favorites

Replace `~/.local/state/opencode/model.json` with:

```json
{
  "recent": [
    {
      "providerID": "opencode",
      "modelID": "deepseek-v4-flash-free"
    },
    {
      "providerID": "openrouter",
      "modelID": "deepseek/deepseek-v4-flash"
    },
    {
      "providerID": "openrouter",
      "modelID": "qwen/qwen3.6-plus"
    }
  ],
  "favorite": [
    {
      "providerID": "openrouter",
      "modelID": "deepseek/deepseek-v4-flash"
    },
    {
      "providerID": "openrouter",
      "modelID": "qwen/qwen3.6-plus"
    },
    {
      "providerID": "openrouter",
      "modelID": "anthropic/claude-3.5-haiku"
    },
    {
      "providerID": "openrouter",
      "modelID": "z-ai/glm-5.1"
    },
    {
      "providerID": "openrouter",
      "modelID": "z-ai/glm-4.5-air:free"
    }
  ],
  "variant": {
    "opencode/deepseek-v4-flash-free": "high"
  }
}
```
