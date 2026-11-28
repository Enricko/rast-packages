-- What the chat bar shows about each reply: the input read back from the
-- provider's cache, the size of what the model read and wrote at its last
-- step (how full its context is), and the time spent writing (for tok/s).
ALTER TABLE ai_messages ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ai_messages ADD COLUMN context_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ai_messages ADD COLUMN writing_ms INTEGER NOT NULL DEFAULT 0;
