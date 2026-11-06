#ifndef BROTYPIST_GRAMMAR_SAMPLER_H
#define BROTYPIST_GRAMMAR_SAMPLER_H

#include "llama.h"
#include <math.h>
#include <stdlib.h>

/* Grammar application decodes every candidate's UTF-8. Before top-k, a bounded
 * raw-logit shortlist is exact when it contains at least k grammar-valid tokens:
 * every omitted logit is lower than every shortlisted logit. Otherwise use the
 * original, untouched array. Ties, NaNs and positive infinity also retain that
 * path. Existing negative-infinity masks cannot outrank finite survivors.
 */
#define BT_GRAMMAR_SHORTLIST 128
struct bt_grammar_sampler {
    struct llama_sampler *grammar;
    int top_k;
    llama_token_data shortlist[BT_GRAMMAR_SHORTLIST];
};

static void bt_grammar_heap_down(llama_token_data *heap, size_t size, size_t root) {
    while (root * 2 + 1 < size) {
        size_t child = root * 2 + 1;
        if (child + 1 < size && heap[child + 1].logit < heap[child].logit) ++child;
        if (heap[root].logit <= heap[child].logit) break;
        llama_token_data swap = heap[root]; heap[root] = heap[child]; heap[child] = swap;
        root = child;
    }
}
static bool bt_grammar_candidates(struct bt_grammar_sampler *ctx, const llama_token_data_array *input) {
    if (input->size <= BT_GRAMMAR_SHORTLIST) return false;
    size_t size = 0;
    float rejected = -INFINITY;
    for (size_t i = 0; i < input->size; ++i) {
        float logit = input->data[i].logit;
        if (isnan(logit) || logit == INFINITY) return false;
        if (logit == -INFINITY) continue;
        if (size < BT_GRAMMAR_SHORTLIST) {
            size_t slot = size++;
            ctx->shortlist[slot] = input->data[i];
            while (slot > 0) {
                size_t parent = (slot - 1) / 2;
                if (ctx->shortlist[parent].logit <= ctx->shortlist[slot].logit) break;
                llama_token_data swap = ctx->shortlist[parent];
                ctx->shortlist[parent] = ctx->shortlist[slot]; ctx->shortlist[slot] = swap;
                slot = parent;
            }
        } else if (logit > ctx->shortlist[0].logit) {
            if (ctx->shortlist[0].logit > rejected) rejected = ctx->shortlist[0].logit;
            ctx->shortlist[0] = input->data[i];
            bt_grammar_heap_down(ctx->shortlist, size, 0);
        } else if (logit > rejected) {
            rejected = logit;
        }
    }
    if (size < BT_GRAMMAR_SHORTLIST || ctx->shortlist[0].logit <= rejected) return false;
    /* Drain the existing min-heap in place; libc qsort may allocate on Linux. */
    for (size_t remaining = size; remaining > 1; --remaining) {
        llama_token_data swap = ctx->shortlist[0];
        ctx->shortlist[0] = ctx->shortlist[remaining - 1];
        ctx->shortlist[remaining - 1] = swap;
        bt_grammar_heap_down(ctx->shortlist, remaining - 1, 0);
    }
    for (size_t i = 1; i < size; ++i) {
        if (ctx->shortlist[i - 1].logit == ctx->shortlist[i].logit) return false;
    }
    return true;
}
static const char *bt_grammar_name(const struct llama_sampler *sampler) {
    (void)sampler;
    return "brotypist-exact-shortlist-grammar";
}
static void bt_grammar_apply(struct llama_sampler *sampler, llama_token_data_array *input) {
    struct bt_grammar_sampler *ctx = sampler->ctx;
    if (bt_grammar_candidates(ctx, input)) {
        /* Grammar can mask a high-ranked token, so this must not claim sorted. */
        llama_token_data_array shortlist = {ctx->shortlist, BT_GRAMMAR_SHORTLIST, -1, false};
        llama_sampler_apply(ctx->grammar, &shortlist);
        size_t valid = 0;
        for (size_t i = 0; i < shortlist.size; ++i) valid += isfinite(shortlist.data[i].logit) != 0;
        if (valid >= (size_t)ctx->top_k) {
            *input = shortlist;
            return;
        }
    }
    /* Pinned llama.cpp grammar apply is const; only accept advances its state. */
    llama_sampler_apply(ctx->grammar, input);
}
static void bt_grammar_accept(struct llama_sampler *sampler, llama_token token) {
    struct bt_grammar_sampler *ctx = sampler->ctx;
    llama_sampler_accept(ctx->grammar, token);
}
static void bt_grammar_reset(struct llama_sampler *sampler) {
    struct bt_grammar_sampler *ctx = sampler->ctx;
    llama_sampler_reset(ctx->grammar);
}
static void bt_grammar_free(struct llama_sampler *sampler) {
    struct bt_grammar_sampler *ctx = sampler->ctx;
    llama_sampler_free(ctx->grammar);
    free(ctx);
}
static struct llama_sampler *bt_grammar_wrap(struct llama_sampler *grammar, int top_k);
static struct llama_sampler *bt_grammar_clone(const struct llama_sampler *sampler) {
    const struct bt_grammar_sampler *ctx = sampler->ctx;
    return bt_grammar_wrap(llama_sampler_clone(ctx->grammar), ctx->top_k);
}
static struct llama_sampler_i bt_grammar_interface = {
    .name = bt_grammar_name, .accept = bt_grammar_accept, .apply = bt_grammar_apply,
    .reset = bt_grammar_reset, .clone = bt_grammar_clone, .free = bt_grammar_free,
};
static struct llama_sampler *bt_grammar_wrap(struct llama_sampler *grammar, int top_k) {
    if (!grammar || top_k <= 0 || top_k >= BT_GRAMMAR_SHORTLIST) return grammar;
    struct bt_grammar_sampler *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) { llama_sampler_free(grammar); return NULL; }
    ctx->grammar = grammar;
    ctx->top_k = top_k;
    struct llama_sampler *sampler = llama_sampler_init(&bt_grammar_interface, ctx);
    if (!sampler) { llama_sampler_free(grammar); free(ctx); }
    return sampler;
}
#endif
