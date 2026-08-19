defmodule Greeter do
  def greet(name) do
    "Hello, #{name}"
  end

  def greet(name, greeting) do
    "#{greeting}, #{name}"
  end
end
