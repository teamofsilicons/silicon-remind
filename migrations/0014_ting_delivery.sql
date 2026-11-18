-- Keep historical receipts intact. Only open occurrences acquire the new delivery channels.
ALTER TABLE accounts ADD COLUMN ting_registered_at timestamptz;
ALTER TABLE executions
    ADD COLUMN ting_status text NOT NULL DEFAULT 'untracked'
        CHECK (ting_status IN ('untracked','pending','accepted','blocked','unsubscribed','disabled','simulated','failed')),
    ADD COLUMN webhook_status text NOT NULL DEFAULT 'untracked'
        CHECK (webhook_status IN ('untracked','pending','delivered','none','simulated','failed'));
ALTER TABLE executions ALTER COLUMN ting_status SET DEFAULT 'pending',
                       ALTER COLUMN webhook_status SET DEFAULT 'pending';
UPDATE executions SET ting_status = 'pending', webhook_status = 'pending' WHERE status IN ('pending','retrying');
ALTER TABLE executions DROP CONSTRAINT executions_delivery_consistent,
    ADD CONSTRAINT executions_delivery_consistent CHECK ((status = 'delivered') = (delivered_at IS NOT NULL));
