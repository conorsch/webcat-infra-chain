# webcat-deployer

Add a new crate to the rust workspace that defines a process orchestrator
for running the primary programs in this project, "felidae" and "cometbft".
The goal is to use this new crate to write an integration test suite.

The new crate should be called "webcat-deployer" and have a binary called same.
It should also expose a library interface, such that integration tests can call
in via rust code, without having to shell out.

The primary representation of the software bundle should be a struct called WebcatNode,
which defines all the necessary ports for the application:

  * 26656/TCP for cometbft p2p
  * 26657/TCP for cometbft api
  * 8080 felidae query port
  * 8081 felidae oracle port

and so on. The WebcatNode should have a "new()" constructor, as well as a Default impl,
that generates a default nodename like "webcat-node-x12346", and all the default ports.

The crucial feature of the "webcat-deployer" logic is that it can support creating
several WebcatNode configs at once, adjusting all their ports carefully such that
they can coexist on the same localhost, for instance, when run as part of an 
integration test suite. 

The interface I'm imagining is:

  $ webcat-deployer create-network --platform local --num-validators 3 --use-sentries=true --directory /tmp/foo

which will create a new network config in /tmp/foo, including all node subdirectories for each node's cometbft and
felidae dirs.

