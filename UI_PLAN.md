# UI plan

The UI currently kinda sucks a little bit. Which is a shame since gpui-kit is pretty awesome. The plan:

Messages in a conversation must lean on https://gpui-kit.com/component/message/, at least for the user massage and attachments

I want to be able to open multiple threads (conversations and factory-runs), in the same way zed supports having multiple editors/terminals etc. To do this I want the same interplay between https://gpui-kit.com/component/dock/ and https://gpui-kit.com/component/tabs/ as zed.  

The promptbox isnt great, maybe we can use https://gpui-kit.com/component/editor/ (without linenumbers though)

more:

https://gpui-kit.com/component/settings/ for the settings we have

And long story short, use appropriate gpui-kit components when possible because they're cool


And a UX that is keyboard-first
