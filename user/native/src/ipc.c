/* Transport: bounded messages, an origin the kernel stamps, and lent authority.
 *
 * `MessageTransport` in `vault/integration/thalyx.md` is this, and the two
 * properties that make it worth porting rather than emulating are both the
 * kernel's: the receiver reads who called from the header the kernel wrote and
 * never from a field of the payload, and a capability travels with the message
 * as a derived grant instead of as a number the receiver has to trust.
 *
 * A payload is at most `MAX_INLINE_PAYLOAD`. That is a small number on purpose
 * and it is not worked around here: anything larger travels in a memory object
 * whose capability the message carries, so a big transfer is visible as an
 * authority rather than hidden inside a copy.
 */

#include "thalyx/nrt.h"
#include <string.h>

int64_t th_call(uint64_t facet, const th_payload *out, th_payload *in,
                uint64_t lend, uint32_t lend_rights, uint64_t deadline_ns)
{
    thalyx_send_request_t request;
    memset(&request, 0, sizeof(request));
    request.payload_len = out ? out->len : 0;
    if (out && out->len) { memcpy(request.payload, out->bytes, out->len); }
    if (lend) {
        uint64_t handle = lend;
        if (lend_rights) {
            thalyx_derive_request_t derive;
            memset(&derive, 0, sizeof(derive));
            derive.rights_mask = lend_rights;
            th_desc d;
            th_desc_begin(&d, THALYX_OP_CAP_DERIVE);
            th_desc_put(&d, TH_BODY, &derive, sizeof(derive));
            th_result r = th_op(lend, THALYX_OP_CAP_DERIVE, &d, 0);
            if (r.status != THALYX_STATUS_OK) { return r.status; }
            handle = r.aux;
        }
        request.caps[0] = handle;
        request.cap_ops[0] = THALYX_CAP_OP_MOVE;
        request.cap_count = 1;
    }

    th_desc d;
    th_desc_begin(&d, THALYX_OP_ENDPOINT_CALL);
    th_desc_put(&d, TH_BODY, &request, sizeof(request));
    th_result r = th_op(facet, THALYX_OP_ENDPOINT_CALL, &d, deadline_ns);
    if (r.status != THALYX_STATUS_OK) { return r.status; }

    thalyx_call_result_t result;
    memcpy(&result, d.bytes + TH_BODY, sizeof(result));
    if (in) {
        in->len = result.payload_len > THALYX_MAX_INLINE_PAYLOAD
                ? THALYX_MAX_INLINE_PAYLOAD : result.payload_len;
        memcpy(in->bytes, result.payload, in->len);
    }
    return (int64_t)result.result;
}

int64_t th_receive(uint64_t endpoint, th_message *out, uint64_t deadline_ns)
{
    th_desc d;
    th_desc_begin(&d, THALYX_OP_ENDPOINT_RECEIVE);
    th_result r = th_op(endpoint, THALYX_OP_ENDPOINT_RECEIVE, &d, deadline_ns);
    if (r.status != THALYX_STATUS_OK) { return r.status; }

    thalyx_receive_result_t received;
    memcpy(&received, d.bytes + TH_BODY, sizeof(received));
    memset(out, 0, sizeof(*out));
    out->header = received.header;
    out->invocation = r.aux;
    out->payload.len = received.header.payload_len > THALYX_MAX_INLINE_PAYLOAD
                     ? THALYX_MAX_INLINE_PAYLOAD : received.header.payload_len;
    memcpy(out->payload.bytes, received.payload, out->payload.len);
    out->lent_count = received.cap_count > THALYX_MAX_CAPS_PER_MESSAGE
                    ? THALYX_MAX_CAPS_PER_MESSAGE : received.cap_count;
    for (unsigned i = 0; i < out->lent_count; i++) { out->lent[i] = received.caps[i]; }
    return THALYX_STATUS_OK;
}

int64_t th_reply(uint64_t invocation, uint64_t result, const th_payload *body)
{
    thalyx_reply_request_t reply;
    memset(&reply, 0, sizeof(reply));
    reply.result = result;
    reply.payload_len = body ? body->len : 0;
    if (body && body->len) { memcpy(reply.payload, body->bytes, body->len); }

    th_desc d;
    th_desc_begin(&d, THALYX_OP_INVOCATION_REPLY);
    th_desc_put(&d, TH_BODY, &reply, sizeof(reply));
    th_result r = th_op(invocation, THALYX_OP_INVOCATION_REPLY, &d, 0);
    return r.status;
}
