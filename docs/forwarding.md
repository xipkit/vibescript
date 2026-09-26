# Dispatch by name

`send`, `public_send` and `respond_to?` were removed by [ADR-008](adr/008-canonical-surface-for-ai-authors.md). With static types a call names its member in the source (V0405), so every call is checked against its signature and private methods and capabilities are reachable only as written. Code that chose a member from data converts the data to an enum once, at the edge, and matches it with `case`, which must name every member or have an `else`:

```vibe
enum Action
  Deposit
  Withdraw
end

class Account
  getter balance: int

  def initialize(@balance: int)
  end

  def apply(action: Action, amount: int) -> int
    @balance = case action
               when Action::Deposit
                 @balance + amount
               when Action::Withdraw
                 @balance - amount
               end
    @balance
  end
end

def parse_action(name: string) -> Action?
  case name
  when "deposit"
    Action::Deposit
  when "withdraw"
    Action::Withdraw
  else
    nil
  end
end

account = Account.new(10)
payload = JSON.parse_as("{\"action\": \"deposit\", \"amount\": 5}", { action: string, amount: int })
action = parse_action(payload["action"])
if action != nil
  account.apply(action, payload["amount"])
end
account.balance # 15
```

Each is reported as V0405; `vibes fix` rewrites a `send` or `public_send` whose name is a symbol literal into the direct call. Capability methods that happen to be named `send`, such as `sms.send(...)`, are ordinary calls and are unaffected.

The [reference differences](forwarding-differences.json) recorded while porting them remain part of the `compatibility` golden corpus.
