### `generate_jlcpcb_order_link` preserves upload outcomes (minor release)

The new order-link tool reports `upload_outcome` as `not_sent`, `sent`, or
`uncertain`. `uploaded` is respectively `false`, `true`, or `null`.

Preflight, request-building and connection failures prove nothing was sent.
HTTP error statuses and response-body read failures are reported as sent with
`http_status`. Other transport failures may happen after the files leave the
machine and are reported as uncertain.

Failures in the upload step retain `order_id`, `order_url: null`, diagnostics
and recovery guidance. A sent result does not prove the upload was accepted or
that a quote exists. After an uncertain result, inspect the previous outcome
before deciding whether to create another upload; do not retry automatically.
