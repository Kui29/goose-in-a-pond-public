FROM golang:1.27.3@sha256:f20d83a83e18dbd2a4714b8ffd4b30b287b6243962dd75608e86abcc63ba4efc AS build
WORKDIR /src
COPY native/pondnet/go.mod native/pondnet/go.sum ./
RUN go mod download
COPY native/pondnet/ ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' -o /pond-enrollment ./cmd/pond-enrollment
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /pond-enrollment /pond-enrollment
ENTRYPOINT ["/pond-enrollment"]
