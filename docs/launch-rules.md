# PWMTF launch rules evidence

This record distinguishes confirmed Miniclip behavior from PWMTF launch-profile decisions. The executable owner is `pwmtf_eight_ball` (on `pwmtf_billiards` physics); this document records the source and interpretation used by its fixtures.

## Confirmed Miniclip behavior

Miniclip's official support article, [How to Move the Cue Ball (8 Ball Pool)](https://support.miniclip.com/hc/en-us/articles/203747476-How-to-Move-the-Cue-Ball-8-Ball-Pool), updated June 6, 2024, states:

- The breaker may move the cue ball only behind the break line.
- After an opponent foul, the incoming player may place the cue ball anywhere on the table.
- Pocketing the cue ball is an example foul.
- Cue-ball contact selection produces spin and changes direction after a rail or ball contact.

Miniclip's official [How to Play 9 Ball](https://support.miniclip.com/hc/en-us/articles/360010436313-How-to-Play-9-Ball) article explicitly identifies the following as shared with its 8-ball rules:

- Failing to contact any object ball is a foul.
- Running out the turn timer is a foul.
- Pocketing the cue ball is a foul.
- After object-ball contact, failing to pocket a legal ball or drive a ball to a rail is a foul.
- An illegal break is a foul.

## PWMTF launch profile decisions

Miniclip's public documentation confirms that an illegal break is a foul but does not publish a numeric rail threshold or deterministic precedence for simultaneous break events. PWMTF version 1 therefore makes the following explicit launch-profile decisions rather than guessing hidden Miniclip behavior:

- A break uses the same objective legality test as any open-table shot: the cue ball must first contact a non-8 object ball and, after contact, either a ball is pocketed or at least one ball reaches a cushion.
- Failure of that test is represented by the existing `NoObjectContact`, `WrongFirstContact`, and `NoRailAfterContact` fouls in their canonical priority order; there is no separate undocumented multi-rail threshold.
- Pocket events are evaluated before the 8-ball break-rerack rule, but fouls remain recorded on the shot result. Pocketing the 8-ball on the break reracks and keeps the breaker selected by the immutable launch profile; it never wins or loses the match.
- Canonical event ordering and tick ordering resolve simultaneous contact, cushion, and pocket evidence; transport or ECS observation order is never used.

Where Miniclip's public support material is incomplete, PWMTF version 1 also uses the binding decisions in `INVARIANTS.md` and the local progress document:

- The table stays open after the break, regardless of groups pocketed during the break.
- First legally pocketed post-break group assigns solids or stripes.
- Only the 8-ball requires a called pocket.
- Pocketing the 8-ball on the break reracks rather than winning.
- A legal 8-ball requires the shooter's group to be cleared, no foul, and the called pocket to match the actual pocket.
- Any other pocketed 8-ball loses the match.
- A timeout ends only the turn, grants the opponent ball-in-hand, and can never complete a match.
- Concession is the only non-8-ball completion reason.
- A linked rematch alternates the breaker.

## Evidence boundary

These are explicit PWMTF version-one rules, not a claim that Miniclip uses the same hidden threshold or precedence. If stronger first-party evidence later establishes different Miniclip behavior, PWMTF must evaluate it as an explicitly versioned rules-profile change and preserve replay of version-one matches. The launch aggregate is complete under the decisions above; no undocumented behavior is inferred at runtime.
