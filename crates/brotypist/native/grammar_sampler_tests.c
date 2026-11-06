/* Native-smoke only. Compare full-vocabulary and shortlisted distributions,
 * including sparse constraints, ties, masked logits and evolving grammar state. */
#include "grammar_sampler.h"
#include <string.h>

struct mask_state { int mode; int phase; };
static struct llama_sampler *mask_new(int mode, int phase);
static const char *mask_name(const struct llama_sampler *sampler) { (void)sampler; return "test-mask"; }
static void mask_apply(struct llama_sampler *sampler, llama_token_data_array *values) {
    struct mask_state *state = sampler->ctx;
    for (size_t i = 0; i < values->size; ++i) {
        int id = values->data[i].id;
        bool allowed = state->mode == 0 ||
            (state->mode == 1 && (id + state->phase) % 3 == 0) ||
            (state->mode == 2 && id < state->phase + 3) ||
            (state->mode == 3 && (id + state->phase) % 2 == 0);
        if (!allowed) values->data[i].logit = -INFINITY;
    }
}
static void mask_accept(struct llama_sampler *sampler, llama_token token) {
    struct mask_state *state = sampler->ctx;
    state->phase = (state->phase + token + 1) % 7;
}
static void mask_reset(struct llama_sampler *sampler) { ((struct mask_state *)sampler->ctx)->phase = 0; }
static void mask_free(struct llama_sampler *sampler) { free(sampler->ctx); }
static struct llama_sampler *mask_clone(const struct llama_sampler *sampler) {
    const struct mask_state *state = sampler->ctx;
    return mask_new(state->mode, state->phase);
}
static struct llama_sampler_i mask_interface = {
    .name = mask_name, .apply = mask_apply, .accept = mask_accept,
    .reset = mask_reset, .clone = mask_clone, .free = mask_free,
};
static struct llama_sampler *mask_new(int mode, int phase) {
    struct mask_state *state = malloc(sizeof(*state));
    if (!state) return NULL;
    *state = (struct mask_state){mode, phase};
    return llama_sampler_init(&mask_interface, state);
}
static struct llama_sampler *test_chain(bool optimized, int mode, int k, unsigned seed) {
    struct llama_sampler *chain = llama_sampler_chain_init(llama_sampler_chain_default_params());
    struct llama_sampler *grammar = mask_new(mode, 0);
    if (optimized) grammar = bt_grammar_wrap(grammar, k);
    llama_sampler_chain_add(chain, grammar);
    llama_sampler_chain_add(chain, llama_sampler_init_top_k(k));
    llama_sampler_chain_add(chain, llama_sampler_init_typical(1.0f, 1));
    llama_sampler_chain_add(chain, llama_sampler_init_top_p(0.95f, 1));
    llama_sampler_chain_add(chain, llama_sampler_init_min_p(0.0f, 1));
    llama_sampler_chain_add(chain, llama_sampler_init_temp(0.35f));
    llama_sampler_chain_add(chain, llama_sampler_init_dist(seed));
    return chain;
}
#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)
int bt_grammar_sampler_regression(void) {
    enum { count = 1024 };
    llama_token_data original[count], optimized[count];
    const int limits[] = {0, 1, 3, 32, 64, 127, 128, 256};
    for (int mode = 0; mode < 4; ++mode) {
        for (size_t limit = 0; limit < sizeof(limits) / sizeof(limits[0]); ++limit) {
            for (int pattern = 0; pattern < 3; ++pattern) {
                for (unsigned seed = 1; seed <= 4; ++seed) {
                    struct llama_sampler *left = test_chain(false, mode, limits[limit], seed);
                    struct llama_sampler *right = test_chain(true, mode, limits[limit], seed);
                    CHECK(left && right);
                    for (int step = 0; step < 5; ++step) {
                        if (step == 2) {
                            struct llama_sampler *a = llama_sampler_clone(left), *b = llama_sampler_clone(right);
                            CHECK(a && b);
                            llama_sampler_free(left); llama_sampler_free(right); left = a; right = b;
                        }
                        if (step == 4) { llama_sampler_reset(left); llama_sampler_reset(right); }
                        for (int i = 0; i < count; ++i) {
                            unsigned rank = (i * 313u + seed * 137u + step * 73u) % count;
                            float logit = (float)(pattern == 1 ? rank / 4 : rank) / 64.0f;
                            if (pattern == 2 && i % 7 == 0) logit = -INFINITY;
                            original[i] = (llama_token_data){i, logit, 0.0f};
                        }
                        memcpy(optimized, original, sizeof(original));
                        llama_token_data_array a = {original, count, -1, false};
                        llama_token_data_array b = {optimized, count, -1, false};
                        llama_sampler_apply(left, &a); llama_sampler_apply(right, &b);
                        CHECK(a.size == b.size && a.selected == b.selected);
                        for (size_t i = 0; i < a.size; ++i) {
                            CHECK(a.data[i].id == b.data[i].id);
                            CHECK(a.data[i].logit == b.data[i].logit);
                            CHECK(a.data[i].p == b.data[i].p);
                        }
                        CHECK(a.selected >= 0 && (size_t)a.selected < a.size);
                        llama_sampler_accept(left, a.data[a.selected].id);
                        llama_sampler_accept(right, b.data[b.selected].id);
                    }
                    llama_sampler_free(left); llama_sampler_free(right);
                }
            }
        }
    }
    /* Isolate a tie only at the omitted boundary: the retained 128 scores are
     * distinct. Also check sparse/small-input fallbacks before top-k can hide
     * an accidental reorder or a substituted candidate buffer. */
    for (int fallback = 0; fallback < 4; ++fallback) {
        size_t size = fallback < 2 ? count : (fallback == 2 ? BT_GRAMMAR_SHORTLIST : 16);
        for (int i = 0; i < count; ++i) {
            original[i] = (llama_token_data){i, fallback == 0 ? 127.0f - i : (float)i, 0};
        }
        if (fallback == 0) original[128].logit = original[127].logit;
        memcpy(optimized, original, sizeof(original));
        int mode = fallback == 1 ? 2 : 0;
        struct llama_sampler *left = mask_new(mode, 0);
        struct llama_sampler *right = bt_grammar_wrap(mask_new(mode, 0), 64);
        llama_token_data_array a = {original, size, -1, false}, b = {optimized, size, -1, false};
        llama_sampler_apply(left, &a); llama_sampler_apply(right, &b);
        CHECK(b.data == optimized && b.size == size && b.sorted == a.sorted);
        for (size_t i = 0; i < size; ++i) {
            CHECK(a.data[i].id == b.data[i].id);
            CHECK(a.data[i].logit == b.data[i].logit);
            CHECK(a.data[i].p == b.data[i].p);
        }
        llama_sampler_free(left); llama_sampler_free(right);
    }
    /* Invalid floating-point inputs must preserve the original grammar path,
     * without asking downstream probabilistic samplers to normalize NaNs. */
    for (int exceptional = 0; exceptional < 2; ++exceptional) {
        for (int i = 0; i < count; ++i) original[i] = (llama_token_data){i, (float)i, 0};
        original[5].logit = exceptional ? INFINITY : NAN;
        memcpy(optimized, original, sizeof(original));
        struct llama_sampler *left = mask_new(1, 0);
        struct llama_sampler *right = bt_grammar_wrap(mask_new(1, 0), 64);
        llama_token_data_array a = {original, count, -1, false}, b = {optimized, count, -1, false};
        llama_sampler_apply(left, &a); llama_sampler_apply(right, &b);
        CHECK(a.size == b.size);
        for (size_t i = 0; i < a.size; ++i) {
            CHECK(a.data[i].id == b.data[i].id);
            CHECK(a.data[i].logit == b.data[i].logit || (isnan(a.data[i].logit) && isnan(b.data[i].logit)));
        }
        llama_sampler_free(left); llama_sampler_free(right);
    }
    return 0;
}
