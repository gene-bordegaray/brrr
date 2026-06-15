This is my implementation of the [1 Billion Row Challenge](https://github.com/gunnarmorling/1brc)
thus far (still working on it for fun during PTO).

I started at over 100s and am sitting at ~1.3s on a macbook M3 chip (I know that isn't the standard
and will update when I run on a proper machine).

I did this to get more experience with the samply profiling tool for optimizations and purposefully
went from a very naive to more involved solution. This was useful in evaluating the samply profile
at each stage and making next optimization decisions at each stage.

I will attach my current samply [here](https://share.firefox.dev/4xvMyaW) for others to check out
for themselves :)
