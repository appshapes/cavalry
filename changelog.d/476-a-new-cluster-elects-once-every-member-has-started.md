- A new cluster now elects its first leader only once every one of its members has
  started. Two members that run while the third has never started wait for it, where
  before they elected one of the two after the 60 s startup canvass timeout. A member
  whose log is empty, beside members whose logs are empty too, votes only once it has
  heard every member, so a member that lost its log directory during the cluster's
  first ballot can no longer vote a second time in that term and leave two leaders of
  one term. Start every member of a new cluster.
  [Run a cluster of three members](docs/how-to/run-a-cluster.md), step 3.
