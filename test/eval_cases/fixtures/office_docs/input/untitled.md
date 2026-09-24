## API 设计草稿 v0.3
- POST /v1/orders 创建订单
- GET /v1/orders/{id} 查询
- 幂等键: Idempotency-Key header
- 错误码统一 RFC7807