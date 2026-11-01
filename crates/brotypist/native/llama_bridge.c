/* llama.cpp ABI pinned to 0c1e57098bba43ac29e6e3b677cdceebdd22334f (MIT).
 * Only opaque pointers, scalars and caller-owned byte buffers cross this ABI.
 * This owner is confined to the inference worker's single model thread.
 */
#include "llama.h"
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#ifdef __APPLE__
#include <math.h>
#import <Foundation/Foundation.h>
#import <NaturalLanguage/NaturalLanguage.h>
#endif

struct bt_llama {
    struct llama_model *model;
    struct llama_context *context;
    struct llama_sampler *sampler;
    const struct llama_vocab *vocab;
    struct llama_batch batch;
    int batch_initialized;
    int backend_initialized;
    int gpu_layers;
};

void bt_llama_sampler_close(struct bt_llama *p) {
    if (p && p->sampler) { llama_sampler_free(p->sampler); p->sampler = NULL; }
}
void bt_llama_close(struct bt_llama *p) {
    if (!p) return;
    bt_llama_sampler_close(p);
    if (p->batch_initialized) llama_batch_free(p->batch);
    if (p->context) llama_free(p->context);
    if (p->model) llama_model_free(p->model);
    if (p->backend_initialized) llama_backend_free();
    free(p);
}
static void quiet_log(enum ggml_log_level level, const char *text, void *user) {
    (void)level; (void)text; (void)user;
}
struct bt_llama *bt_llama_open(const char *path, int auto_gpu, int threads, char *error, size_t cap) {
    struct bt_llama *p = calloc(1, sizeof(*p));
    if (!p) { snprintf(error, cap, "Native model owner allocation failed."); return NULL; }
    llama_log_set(quiet_log, NULL);
    llama_backend_init(); p->backend_initialized = 1;
    p->gpu_layers = auto_gpu && llama_supports_gpu_offload() ? -1 : 0;
    struct llama_model_params mp = llama_model_default_params();
    mp.n_gpu_layers = p->gpu_layers;
    p->model = llama_model_load_from_file(path, mp);
    if (!p->model) { snprintf(error, cap, "The native engine could not load this GGUF model."); goto fail; }
    if (llama_model_has_encoder(p->model) || !llama_model_has_decoder(p->model)) {
        snprintf(error, cap, "Local writing requires a decoder-only GGUF model."); goto fail;
    }
    p->vocab = llama_model_get_vocab(p->model);
    if (!p->vocab) { snprintf(error, cap, "The GGUF model has no vocabulary."); goto fail; }
    struct llama_context_params cp = llama_context_default_params();
    cp.n_ctx = 4096; cp.n_batch = 64; cp.n_ubatch = 32;
    cp.n_threads = threads > 1 ? threads / 2 : 1;
    cp.n_threads_batch = threads > 0 ? threads : 1;
    cp.flash_attn_type = LLAMA_FLASH_ATTN_TYPE_DISABLED;
    if (!auto_gpu) { cp.offload_kqv = false; cp.op_offload = false; }
    p->context = llama_init_from_model(p->model, cp);
    if (!p->context) { snprintf(error, cap, "The native engine could not allocate the model context."); goto fail; }
    p->batch = llama_batch_init(64, 0, 1); p->batch_initialized = 1;
    if (!p->batch.token || !p->batch.pos || !p->batch.n_seq_id || !p->batch.seq_id || !p->batch.logits) {
        snprintf(error, cap, "Native inference batch allocation failed."); goto fail;
    }
    return p;
fail:
    bt_llama_close(p); return NULL;
}
int bt_llama_context_size(const struct bt_llama *p) { return (int)llama_n_ctx(p->context); }
int bt_llama_gpu_layers(const struct bt_llama *p) { return p->gpu_layers; }
int bt_llama_meta(const struct bt_llama *p, const char *key, char *buf, size_t cap) {
    return llama_model_meta_val_str(p->model, key, buf, cap);
}
int bt_llama_tokenize(const struct bt_llama *p, const char *text, int32_t length, int32_t *tokens, int32_t cap, int add_bos, int special) {
    return llama_tokenize(p->vocab, text, length, tokens, cap, add_bos != 0, special != 0);
}
int bt_llama_piece(const struct bt_llama *p, int32_t token, char *bytes, int32_t cap) {
    return llama_token_to_piece(p->vocab, token, bytes, cap, 0, true);
}
int bt_llama_is_eog(const struct bt_llama *p, int32_t token) { return llama_vocab_is_eog(p->vocab, token); }
int bt_llama_trim(struct bt_llama *p, int32_t from) {
    return llama_memory_seq_rm(llama_get_memory(p->context), -1, from, -1);
}
void bt_llama_reset(struct bt_llama *p) { llama_memory_clear(llama_get_memory(p->context), true); }
int bt_llama_decode(struct bt_llama *p, const int32_t *tokens, int32_t count, int32_t start_pos) {
    if (count < 1 || count > 64 || start_pos < 0 || start_pos + count > (int32_t)llama_n_ctx(p->context)) return -100;
    struct llama_batch batch = p->batch;
    batch.n_tokens = count;
    for (int32_t i = 0; i < count; ++i) {
        batch.token[i] = tokens[i]; batch.pos[i] = start_pos + i;
        batch.n_seq_id[i] = 1; batch.seq_id[i][0] = 0; batch.logits[i] = i == count - 1;
    }
    return llama_decode(p->context, batch);
}
static int add_sampler(struct bt_llama *p, struct llama_sampler *sampler) {
    if (!sampler) return -1;
    llama_sampler_chain_add(p->sampler, sampler); return 0;
}
int bt_llama_sampler(struct bt_llama *p, int top_k, float top_p, float min_p, float temperature,
                     float repeat_penalty, uint32_t seed, const char *grammar) {
    bt_llama_sampler_close(p);
    p->sampler = llama_sampler_chain_init(llama_sampler_chain_default_params());
    if (!p->sampler) return -1;
    /* Exact llama-cpp-python v0.3.36 regular sampler order/defaults.
     * Prompt tokens are NOT accepted into the sampler. Grammar consumes only
     * sampled output (accepting the draft would corrupt exact replay grammar).
     */
    if (add_sampler(p, llama_sampler_init_penalties(llama_vocab_n_tokens(p->vocab), 64, repeat_penalty, 0.0f, 0.0f))) goto fail;
    if (grammar && grammar[0] && add_sampler(p, llama_sampler_init_grammar(p->vocab, grammar, "root"))) goto fail;
    if (add_sampler(p, llama_sampler_init_top_k(top_k)) ||
        add_sampler(p, llama_sampler_init_typical(1.0f, 1)) ||
        add_sampler(p, llama_sampler_init_top_p(top_p, 1)) ||
        add_sampler(p, llama_sampler_init_min_p(min_p, 1)) ||
        add_sampler(p, llama_sampler_init_temp(temperature)) ||
        add_sampler(p, llama_sampler_init_dist(seed))) goto fail;
    return 0;
fail:
    bt_llama_sampler_close(p); return -1;
}
int32_t bt_llama_sample(struct bt_llama *p) {
    /* llama_sampler_sample accepts its own selected output token exactly once. */
    return llama_sampler_sample(p->sampler, p->context, -1);
}

/* Optional macOS evidence; a fresh recognizer belongs only to this call. */
int bt_draft_language_name(const char *text, size_t length, char *out, size_t cap) {
#ifdef __APPLE__
    @autoreleasepool {
        @try {
            if (![NLLanguageRecognizer class]) return 0;
            NSString *draft = [[[NSString alloc] initWithBytes:text length:length encoding:NSUTF8StringEncoding] autorelease];
            if (!draft) return 0;
            NLLanguageRecognizer *recognizer = [[[NLLanguageRecognizer alloc] init] autorelease];
            if (!recognizer) return 0;
            [recognizer reset]; [recognizer processString:draft];
            NSDictionary *hypotheses = [recognizer languageHypothesesWithMaximum:5];
            NSString *tag = nil; double first = -1.0, second = 0.0;
            for (NSString *key in hypotheses) {
                double score = [[hypotheses objectForKey:key] doubleValue];
                if (!isfinite(score)) continue;
                if (score > first) { second = first > 0.0 ? first : 0.0; first = score; tag = key; }
                else if (score > second) second = score;
            }
            int result = 0;
            if (tag && first >= 0.90 && first - second >= 0.80 && ![tag isEqualToString:@"und"]) {
                NSRegularExpression *pattern = [NSRegularExpression regularExpressionWithPattern:@"^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$" options:0 error:NULL];
                if ([pattern numberOfMatchesInString:tag options:0 range:NSMakeRange(0, [tag length])] == 1) {
                    NSLocale *english = [[[NSLocale alloc] initWithLocaleIdentifier:@"en"] autorelease];
                    NSString *name = [english displayNameForKey:NSLocaleLanguageCode value:tag];
                    const char *bytes = [name UTF8String];
                    if (bytes && strlen(bytes) < cap) { result = (int)strlen(bytes); memcpy(out, bytes, (size_t)result); out[result] = 0; }
                }
            }
            [recognizer reset]; return result;
        } @catch (NSException *exception) { (void)exception; return 0; }
    }
#else
    (void)text; (void)length; (void)out; (void)cap; return 0;
#endif
}
