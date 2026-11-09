<div align="center">

# CheatIndicatorAlwaysGreen

<img src="https://i.imgur.com/SLSxadT.png"></img>

Small Rust utility to patch MegaHack v9 memory so the Cheat Indicator never becomes red.
</div>


## How

<img src="https://i.imgur.com/4XY5Sia.png"></img>

The program patches areCheatsEnabled(), hasCheatedInAttempt() and the function related to returning colors; Safe Mode might be broken

Requires: Windows x64 and MegaHack v9.2.0-alpha.5, other versions of MegaHack are not tested,


## Building

You're gonna need Rust stable, MSVC Build Tools C++ and Windows SDK. Do the following in x64 Native Tools Command Prompt from project's path:

```text
cargo build --release --locked
```


## Running
You can download a premade build from Releases

Launch the game, then the patcher and click "Turn on", you can also close the patcher after that. If you need to turn it off just open it once again and click "Turn off", its that simple.

