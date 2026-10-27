#ifndef apple_intelligence_bridge_h
#include <stdint.h>
#define apple_intelligence_bridge_h

// C-compatible function declarations for Swift bridge

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char* response;
    int success; // 0 for failure, 1 for success
    char* error_message; // Only valid when success = 0
} AppleLLMResponse;

// Why Apple Intelligence can or cannot answer right now. The OS distinguishes
// three unavailability reasons and they call for different user action, so the
// bridge reports the reason rather than a bare yes/no.
//   0 available, 1 switched off in System Settings, 2 model still downloading,
//   3 device not eligible, 4 macOS older than 26, 5 reason unknown to us.
int apple_intelligence_status(void);

typedef void (*AppleLLMCompletion)(uint64_t request_id, AppleLLMResponse* response);
#define APPLE_LLM_RESPONSE_TEXT 0
#define APPLE_LLM_RESPONSE_JSON 1
#define APPLE_LLM_RESPONSE_MEETING_NOTES 2
#define APPLE_LLM_RESPONSE_MEETING_LEDGER 3
void start_apple_intelligence_generation(uint64_t request_id, const char* system_prompt, const char* user_content, int max_tokens, int response_shape, const char* json_schema, AppleLLMCompletion completion);
void cancel_apple_intelligence_generation(uint64_t request_id);

// Free memory allocated by the Apple LLM response
void free_apple_llm_response(AppleLLMResponse* response);

#ifdef __cplusplus
}
#endif

#endif /* apple_intelligence_bridge_h */